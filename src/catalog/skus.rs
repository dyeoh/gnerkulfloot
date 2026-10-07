//! SKUs: the buyable variants of a product, with their prices and stock.
//!
//! Stock is only ever changed by relative adjustments (`+5`, `-2`), never set
//! to an absolute number. `stock_available` already has units held by unpaid
//! orders taken off, so "set stock to 10 after a recount" would silently
//! release those holds and oversell.

use std::collections::{HashMap, HashSet};

use iso_currency::Currency;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sqlx::{PgConnection, PgPool};
use uuid::Uuid;

use super::{CatalogError, map_unique};

/// A SKU's price in one currency, in minor units.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SkuPrice {
    pub currency: Currency,
    pub amount: i64,
    /// Optional "was" price, shown struck through.
    pub compare_at_amount: Option<i64>,
}

#[derive(Debug, Serialize)]
pub struct Sku {
    pub id: Uuid,
    pub product_id: Uuid,
    pub code: String,
    pub name: String,
    pub options: Value,
    pub stock_available: i32,
    pub weight_g: i32,
    pub active: bool,
    pub position: i32,
    pub prices: Vec<SkuPrice>,
}

#[derive(Deserialize)]
pub struct NewSku {
    pub code: String,
    #[serde(default)]
    pub name: String,
    #[serde(default = "empty_object")]
    pub options: Value,
    /// Opening stock.
    #[serde(default)]
    pub stock_available: i32,
    #[serde(default)]
    pub weight_g: i32,
    #[serde(default = "yes")]
    pub active: bool,
    #[serde(default)]
    pub position: i32,
    #[serde(default)]
    pub prices: Vec<SkuPrice>,
}

fn empty_object() -> Value {
    Value::Object(Default::default())
}

fn yes() -> bool {
    true
}

/// Partial update. Stock isn't here on purpose; see the module docs.
#[derive(Deserialize)]
pub struct SkuPatch {
    pub code: Option<String>,
    pub name: Option<String>,
    pub options: Option<Value>,
    pub weight_g: Option<i32>,
    pub active: Option<bool>,
    pub position: Option<i32>,
    /// Replaces all prices when present.
    pub prices: Option<Vec<SkuPrice>>,
}

fn validate_code(code: &str) -> Result<String, CatalogError> {
    let code = code.trim();
    if code.is_empty() || code.len() > 64 || code.chars().any(char::is_whitespace) {
        return Err(CatalogError::InvalidInput(
            "SKU code must be 1–64 characters with no spaces".into(),
        ));
    }
    Ok(code.to_owned())
}

fn validate_prices(prices: &[SkuPrice]) -> Result<(), CatalogError> {
    let mut seen = HashSet::new();
    for p in prices {
        if !seen.insert(p.currency) {
            return Err(CatalogError::InvalidInput(format!(
                "{} is priced twice",
                p.currency.code()
            )));
        }
        if p.amount < 0 || p.compare_at_amount.is_some_and(|c| c < 0) {
            return Err(CatalogError::InvalidInput("prices can't be negative".into()));
        }
    }
    Ok(())
}

fn validate_options(options: &Value) -> Result<(), CatalogError> {
    if options.is_object() {
        Ok(())
    } else {
        Err(CatalogError::InvalidInput("options must be a JSON object".into()))
    }
}

fn validate_non_negative(field: &str, value: i32) -> Result<(), CatalogError> {
    if value < 0 {
        return Err(CatalogError::InvalidInput(format!("{field} can't be negative")));
    }
    Ok(())
}

pub async fn create(db: &PgPool, product_id: Uuid, new: NewSku) -> Result<Sku, CatalogError> {
    let code = validate_code(&new.code)?;
    validate_options(&new.options)?;
    validate_prices(&new.prices)?;
    validate_non_negative("stock_available", new.stock_available)?;
    validate_non_negative("weight_g", new.weight_g)?;

    let mut tx = db.begin().await?;
    let product_exists = sqlx::query_scalar!(
        r#"SELECT EXISTS (SELECT 1 FROM products WHERE id = $1) AS "e!""#,
        product_id
    )
    .fetch_one(&mut *tx)
    .await?;
    if !product_exists {
        return Err(CatalogError::NotFound);
    }
    let id = Uuid::now_v7();
    sqlx::query!(
        "INSERT INTO skus (id, product_id, code, name, options, stock_available, weight_g, active, position)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)",
        id,
        product_id,
        code,
        new.name.trim(),
        new.options,
        new.stock_available,
        new.weight_g,
        new.active,
        new.position,
    )
    .execute(&mut *tx)
    .await
    .map_err(map_unique)?;
    replace_prices(&mut tx, id, &new.prices).await?;
    tx.commit().await?;
    get(db, id).await
}

pub async fn update(db: &PgPool, id: Uuid, patch: SkuPatch) -> Result<Sku, CatalogError> {
    let code = patch.code.as_deref().map(validate_code).transpose()?;
    if let Some(options) = &patch.options {
        validate_options(options)?;
    }
    if let Some(prices) = &patch.prices {
        validate_prices(prices)?;
    }
    if let Some(w) = patch.weight_g {
        validate_non_negative("weight_g", w)?;
    }

    let mut tx = db.begin().await?;
    let updated = sqlx::query!(
        "UPDATE skus SET
             code       = COALESCE($2, code),
             name       = COALESCE($3, name),
             options    = COALESCE($4, options),
             weight_g   = COALESCE($5, weight_g),
             active     = COALESCE($6, active),
             position   = COALESCE($7, position),
             updated_at = now()
         WHERE id = $1",
        id,
        code,
        patch.name.as_deref().map(str::trim),
        patch.options,
        patch.weight_g,
        patch.active,
        patch.position,
    )
    .execute(&mut *tx)
    .await
    .map_err(map_unique)?;
    if updated.rows_affected() == 0 {
        return Err(CatalogError::NotFound);
    }
    if let Some(prices) = &patch.prices {
        replace_prices(&mut tx, id, prices).await?;
    }
    tx.commit().await?;
    get(db, id).await
}

async fn replace_prices(conn: &mut PgConnection, sku_id: Uuid, prices: &[SkuPrice]) -> Result<(), sqlx::Error> {
    sqlx::query!("DELETE FROM sku_prices WHERE sku_id = $1", sku_id)
        .execute(&mut *conn)
        .await?;
    for p in prices {
        sqlx::query!(
            "INSERT INTO sku_prices (sku_id, price_currency, price_amount, compare_at_amount) VALUES ($1, $2, $3, $4)",
            sku_id,
            p.currency.code(),
            p.amount,
            p.compare_at_amount,
        )
        .execute(&mut *conn)
        .await?;
    }
    Ok(())
}

/// Adds `delta` units to stock (negative to remove), atomically.
///
/// # Errors
/// `InsufficientStock` if removing more than is available, `NotFound` for an unknown SKU.
pub async fn adjust_stock(db: &PgPool, id: Uuid, delta: i32) -> Result<i32, CatalogError> {
    // The WHERE clause makes check-and-update a single atomic step, so two
    // concurrent adjustments can never take stock below zero.
    let new_level = sqlx::query_scalar!(
        "UPDATE skus SET stock_available = stock_available + $2, updated_at = now()
         WHERE id = $1 AND stock_available + $2 >= 0
         RETURNING stock_available",
        id,
        delta
    )
    .fetch_optional(db)
    .await?;
    if let Some(level) = new_level {
        return Ok(level);
    }
    let available = sqlx::query_scalar!("SELECT stock_available FROM skus WHERE id = $1", id)
        .fetch_optional(db)
        .await?
        .ok_or(CatalogError::NotFound)?;
    Err(CatalogError::InsufficientStock { available })
}

pub async fn get(db: &PgPool, id: Uuid) -> Result<Sku, CatalogError> {
    let mut skus = load(db, LoadBy::Sku(id)).await?;
    skus.pop().ok_or(CatalogError::NotFound)
}

pub async fn for_product(db: &PgPool, product_id: Uuid) -> Result<Vec<Sku>, sqlx::Error> {
    load(db, LoadBy::Product(product_id)).await
}

enum LoadBy {
    Sku(Uuid),
    Product(Uuid),
}

async fn load(db: &PgPool, by: LoadBy) -> Result<Vec<Sku>, sqlx::Error> {
    let (sku_id, product_id) = match by {
        LoadBy::Sku(id) => (Some(id), None),
        LoadBy::Product(id) => (None, Some(id)),
    };
    let rows = sqlx::query!(
        "SELECT id, product_id, code, name, options, stock_available, weight_g, active, position FROM skus
         WHERE ($1::uuid IS NULL OR id = $1) AND ($2::uuid IS NULL OR product_id = $2)
         ORDER BY position, created_at",
        sku_id,
        product_id
    )
    .fetch_all(db)
    .await?;
    let ids: Vec<Uuid> = rows.iter().map(|r| r.id).collect();
    let mut prices: HashMap<Uuid, Vec<SkuPrice>> = HashMap::new();
    for p in sqlx::query!(
        "SELECT sku_id, price_currency, price_amount, compare_at_amount FROM sku_prices
         WHERE sku_id = ANY($1) ORDER BY price_currency",
        &ids
    )
    .fetch_all(db)
    .await?
    {
        // Only codes we wrote ourselves are stored, so an unknown one means a manual edit; skip it.
        if let Some(currency) = Currency::from_code(&p.price_currency) {
            prices.entry(p.sku_id).or_default().push(SkuPrice {
                currency,
                amount: p.price_amount,
                compare_at_amount: p.compare_at_amount,
            });
        }
    }
    Ok(rows
        .into_iter()
        .map(|r| Sku {
            prices: prices.remove(&r.id).unwrap_or_default(),
            id: r.id,
            product_id: r.product_id,
            code: r.code,
            name: r.name,
            options: r.options,
            stock_available: r.stock_available,
            weight_g: r.weight_g,
            active: r.active,
            position: r.position,
        })
        .collect())
}
