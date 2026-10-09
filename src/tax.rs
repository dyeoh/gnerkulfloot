//! Tax: which rate applies to a destination, and how much tax an amount
//! carries.
//!
//! The most specific `tax_rules` row for the destination wins (postcode prefix
//! over state over country), else the configured default. A payment adapter
//! that calculates tax itself will be asked first once one exists.

use serde::{Deserialize, Serialize};
use sqlx::PgPool;
use uuid::Uuid;

use crate::{
    config::TaxConfig,
    error::DataError,
    money::{Money, MoneyError, Rounding},
    shipping::Destination,
};

const BP_PER_WHOLE: i64 = 10_000;

/// The tax that applies to one order.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct TaxRate {
    pub name: String,
    /// Hundredths of a percent: 6% = 600.
    pub rate_bp: i32,
    pub applies_to_shipping: bool,
}

/// Finds the rate for a (normalized) destination.
pub async fn resolve(db: &PgPool, cfg: &TaxConfig, dest: &Destination) -> Result<TaxRate, sqlx::Error> {
    let rule = sqlx::query!(
        "SELECT name, rate_bp, applies_to_shipping FROM tax_rules
         WHERE country = $1
           AND (state = '' OR state = $2)
           AND starts_with($3, postcode_prefix)
         ORDER BY length(postcode_prefix) DESC, (state <> '') DESC
         LIMIT 1",
        dest.country,
        dest.state,
        dest.postcode,
    )
    .fetch_optional(db)
    .await?;
    Ok(match rule {
        Some(r) => TaxRate {
            name: r.name,
            rate_bp: r.rate_bp,
            applies_to_shipping: r.applies_to_shipping,
        },
        None => TaxRate {
            name: cfg.default_name.clone(),
            rate_bp: cfg.default_rate_bp,
            applies_to_shipping: false,
        },
    })
}

/// The tax carried by `amount` at `rate_bp`.
///
/// With `inclusive` prices the tax is the part of `amount` that is tax
/// (RM10.60 at 6% carries RM0.60); otherwise it's added on top (RM10.00 at 6%
/// adds RM0.60).
///
/// # Examples
/// ```
/// use gnerkulfloot::{money::{Money, Rounding}, tax::tax_on};
/// use iso_currency::Currency;
///
/// let on_top = tax_on(Money::new(1000, Currency::MYR), 600, false, Rounding::HalfUp).unwrap();
/// assert_eq!(on_top.amount, 60);
/// let included = tax_on(Money::new(1060, Currency::MYR), 600, true, Rounding::HalfUp).unwrap();
/// assert_eq!(included.amount, 60);
/// ```
pub fn tax_on(amount: Money, rate_bp: i32, inclusive: bool, rounding: Rounding) -> Result<Money, MoneyError> {
    let bp = i64::from(rate_bp);
    if inclusive {
        let net = amount.apply_rate(BP_PER_WHOLE, BP_PER_WHOLE + bp, rounding)?;
        amount.checked_sub(net)
    } else {
        amount.apply_rate(bp, BP_PER_WHOLE, rounding)
    }
}

#[derive(Debug, Serialize, utoipa::ToSchema)]
pub struct TaxRule {
    pub id: Uuid,
    pub name: String,
    /// ISO 3166-1 alpha-2, e.g. `MY`.
    pub country: String,
    pub state: String,
    /// Matches postcodes starting with this; empty matches all.
    pub postcode_prefix: String,
    /// Hundredths of a percent: 6% is 600.
    pub rate_bp: i32,
    pub applies_to_shipping: bool,
}

#[derive(Deserialize, utoipa::ToSchema)]
pub struct NewTaxRule {
    pub name: String,
    /// ISO 3166-1 alpha-2, e.g. `MY`.
    pub country: String,
    #[serde(default)]
    pub state: String,
    /// Matches postcodes starting with this; empty matches all.
    #[serde(default)]
    pub postcode_prefix: String,
    /// Hundredths of a percent: 6% is 600.
    pub rate_bp: i32,
    #[serde(default)]
    pub applies_to_shipping: bool,
}

#[derive(Deserialize, utoipa::ToSchema)]
pub struct TaxRulePatch {
    pub name: Option<String>,
    /// Hundredths of a percent: 6% is 600.
    pub rate_bp: Option<i32>,
    pub applies_to_shipping: Option<bool>,
}

fn validate_rate(rate_bp: i32) -> Result<(), DataError> {
    if (0..=10_000).contains(&rate_bp) {
        Ok(())
    } else {
        Err(DataError::InvalidInput(
            "rate_bp must be between 0 and 10000 (0–100%)".into(),
        ))
    }
}

pub async fn list_rules(db: &PgPool) -> Result<Vec<TaxRule>, sqlx::Error> {
    sqlx::query_as!(
        TaxRule,
        "SELECT id, name, country, state, postcode_prefix, rate_bp, applies_to_shipping
         FROM tax_rules ORDER BY country, state, postcode_prefix"
    )
    .fetch_all(db)
    .await
}

pub async fn create_rule(db: &PgPool, r: NewTaxRule) -> Result<TaxRule, DataError> {
    let dest = Destination {
        country: r.country,
        state: r.state,
        postcode: r.postcode_prefix,
    }
    .normalized()
    .map_err(DataError::InvalidInput)?;
    if !dest.postcode.chars().all(|c| c.is_ascii_alphanumeric()) {
        return Err(DataError::InvalidInput(
            "postcode_prefix may only contain letters and digits".into(),
        ));
    }
    validate_rate(r.rate_bp)?;
    let name = r.name.trim();
    if name.is_empty() {
        return Err(DataError::InvalidInput("name is required".into()));
    }
    sqlx::query_as!(
        TaxRule,
        "INSERT INTO tax_rules (id, name, country, state, postcode_prefix, rate_bp, applies_to_shipping)
         VALUES ($1, $2, $3, $4, $5, $6, $7)
         RETURNING id, name, country, state, postcode_prefix, rate_bp, applies_to_shipping",
        Uuid::now_v7(),
        name,
        dest.country,
        dest.state,
        dest.postcode,
        r.rate_bp,
        r.applies_to_shipping,
    )
    .fetch_one(db)
    .await
    .map_err(|e| match e {
        sqlx::Error::Database(d) if d.is_unique_violation() => {
            DataError::Conflict("a rule for that country, state and postcode prefix already exists".into())
        }
        other => other.into(),
    })
}

pub async fn update_rule(db: &PgPool, id: Uuid, p: TaxRulePatch) -> Result<TaxRule, DataError> {
    if let Some(bp) = p.rate_bp {
        validate_rate(bp)?;
    }
    sqlx::query_as!(
        TaxRule,
        "UPDATE tax_rules SET
             name                = COALESCE($2, name),
             rate_bp             = COALESCE($3, rate_bp),
             applies_to_shipping = COALESCE($4, applies_to_shipping),
             updated_at          = now()
         WHERE id = $1
         RETURNING id, name, country, state, postcode_prefix, rate_bp, applies_to_shipping",
        id,
        p.name.as_deref().map(str::trim),
        p.rate_bp,
        p.applies_to_shipping,
    )
    .fetch_optional(db)
    .await?
    .ok_or(DataError::NotFound)
}

pub async fn delete_rule(db: &PgPool, id: Uuid) -> Result<(), DataError> {
    let deleted = sqlx::query!("DELETE FROM tax_rules WHERE id = $1", id)
        .execute(db)
        .await?;
    if deleted.rows_affected() == 0 {
        return Err(DataError::NotFound);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use iso_currency::Currency::{JPY, MYR};

    #[test]
    fn exclusive_and_inclusive_tax() {
        let r = Rounding::HalfUp;
        assert_eq!(tax_on(Money::new(1999, MYR), 600, false, r).unwrap().amount, 120); // 119.94
        assert_eq!(tax_on(Money::new(2119, MYR), 600, true, r).unwrap().amount, 120); // 2119 - 1999.06
        assert_eq!(tax_on(Money::new(1000, MYR), 0, true, r).unwrap().amount, 0);
        assert_eq!(tax_on(Money::new(1000, JPY), 1000, false, r).unwrap().amount, 100);
        // 8.25% exactly, no float drift.
        assert_eq!(tax_on(Money::new(10_000, MYR), 825, false, r).unwrap().amount, 825);
    }
}
