//! Shipping: the `ShippingAdapter` trait every carrier integration
//! implements, and the address types checkout uses.
//!
//! Checkout asks every enabled adapter for options and merges them. Option ids
//! are prefixed with the adapter's id (`flat_rate:…`), so an order can be
//! re-quoted and its chosen option found again without trusting the client's
//! price.

mod flat_rate;
pub mod zones;

use std::sync::Arc;

use async_trait::async_trait;
use iso_currency::Currency;
use serde::{Deserialize, Serialize};
use sqlx::PgPool;

pub use flat_rate::FlatRate;

use crate::{config::ShippingAdapterKind, money::Money};

/// Where a parcel is going, as much as rate lookup needs.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq, utoipa::ToSchema)]
pub struct Destination {
    /// ISO 3166-1 alpha-2, e.g. `MY`.
    pub country: String,
    #[serde(default)]
    pub state: String,
    #[serde(default)]
    pub postcode: String,
}

impl Destination {
    /// Uppercases and trims, and checks the country code looks right.
    pub fn normalized(&self) -> Result<Destination, String> {
        let country = self.country.trim().to_ascii_uppercase();
        if country.len() != 2 || !country.chars().all(|c| c.is_ascii_alphabetic()) {
            return Err("country must be a two-letter ISO code, e.g. MY".into());
        }
        Ok(Destination {
            country,
            state: self.state.trim().to_uppercase(),
            postcode: self.postcode.trim().to_uppercase(),
        })
    }
}

/// What an adapter needs to price a shipment.
#[derive(Clone, Debug)]
pub struct ShipmentRequest {
    pub currency: Currency,
    pub destination: Destination,
    pub weight_g: i64,
    /// Order subtotal, for free-shipping thresholds.
    pub subtotal: Money,
}

/// One way to ship an order, with its price.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq, utoipa::ToSchema)]
pub struct ShippingOption {
    /// Stable id the client sends back to choose this option.
    pub id: String,
    pub adapter: String,
    pub name: String,
    pub price: Money,
}

#[derive(Debug, thiserror::Error)]
pub enum ShippingError {
    #[error(transparent)]
    Database(#[from] sqlx::Error),
    #[error("shipping provider error: {0}")]
    Provider(String),
}

#[async_trait]
pub trait ShippingAdapter: Send + Sync {
    /// Short id, used as the prefix of this adapter's option ids.
    fn id(&self) -> &'static str;

    /// Options for this shipment, empty when the adapter can't ship it
    /// (destination not covered, no rate in this currency, too heavy).
    async fn quote(&self, req: &ShipmentRequest) -> Result<Vec<ShippingOption>, ShippingError>;
}

/// Builds the adapters enabled in config.
pub fn from_config(kinds: &[ShippingAdapterKind], db: &PgPool) -> Vec<Arc<dyn ShippingAdapter>> {
    kinds
        .iter()
        .map(|kind| match kind {
            ShippingAdapterKind::FlatRate => Arc::new(FlatRate::new(db.clone())) as Arc<dyn ShippingAdapter>,
        })
        .collect()
}

/// Asks every adapter for options and merges them, cheapest first.
pub async fn quote_all(
    adapters: &[Arc<dyn ShippingAdapter>],
    req: &ShipmentRequest,
) -> Result<Vec<ShippingOption>, ShippingError> {
    let mut options = Vec::new();
    for adapter in adapters {
        options.extend(adapter.quote(req).await?);
    }
    options.sort_by(|a, b| a.price.amount.cmp(&b.price.amount).then_with(|| a.name.cmp(&b.name)));
    Ok(options)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn destinations_normalize() {
        let d = Destination {
            country: " my ".into(),
            state: "sabah".into(),
            postcode: "88000 ".into(),
        };
        let n = d.normalized().unwrap();
        assert_eq!(
            (n.country.as_str(), n.state.as_str(), n.postcode.as_str()),
            ("MY", "SABAH", "88000")
        );
        let bad = Destination {
            country: "Malaysia".into(),
            state: String::new(),
            postcode: String::new(),
        };
        assert!(bad.normalized().is_err());
    }
}
