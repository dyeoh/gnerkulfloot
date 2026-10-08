//! The `flat_rate` adapter: rates from our own zone and rate tables, managed
//! through the admin API. No outside service involved.

use async_trait::async_trait;
use sqlx::PgPool;

use super::{ShipmentRequest, ShippingAdapter, ShippingError, ShippingOption};
use crate::money::Money;

pub struct FlatRate {
    db: PgPool,
}

impl FlatRate {
    pub fn new(db: PgPool) -> Self {
        Self { db }
    }
}

#[async_trait]
impl ShippingAdapter for FlatRate {
    fn id(&self) -> &'static str {
        "flat_rate"
    }

    /// A zone matches the destination's state or its whole country. When a
    /// state-level zone matches, country-wide zones are ignored, so "East
    /// Malaysia" overrides a catch-all "Malaysia".
    async fn quote(&self, req: &ShipmentRequest) -> Result<Vec<ShippingOption>, ShippingError> {
        let rows = sqlx::query!(
            r#"WITH matching AS (
                   SELECT DISTINCT r.zone_id, (r.state <> '') AS by_state
                   FROM shipping_zone_regions r
                   WHERE r.country = $1 AND (r.state = '' OR r.state = $2)
               ),
               best AS (
                   SELECT zone_id FROM matching
                   WHERE by_state = (SELECT bool_or(by_state) FROM matching)
               )
               SELECT sr.id, sr.name, sr.price_amount, sr.free_over_amount
               FROM shipping_rates sr JOIN best ON best.zone_id = sr.zone_id
               WHERE sr.active
                 AND sr.price_currency = $3
                 AND sr.min_weight_g <= $4
                 AND (sr.max_weight_g IS NULL OR sr.max_weight_g >= $4)
               ORDER BY sr.position, sr.price_amount"#,
            req.destination.country,
            req.destination.state,
            req.currency.code(),
            req.weight_g as i32,
        )
        .fetch_all(&self.db)
        .await?;

        Ok(rows
            .into_iter()
            .map(|r| {
                let free = r.free_over_amount.is_some_and(|t| req.subtotal.amount >= t);
                ShippingOption {
                    id: format!("{}:{}", self.id(), r.id),
                    adapter: self.id().into(),
                    name: r.name,
                    price: Money::new(if free { 0 } else { r.price_amount }, req.currency),
                }
            })
            .collect())
    }
}
