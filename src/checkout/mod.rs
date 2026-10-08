//! Checkout: pricing a basket ([`quote`]), placing orders that hold stock
//! ([`orders`], [`inventory`]), and releasing that stock when an unpaid order
//! expires or is cancelled.
//!
//! Prices, shipping and tax always come from the server. A client sends only
//! SKU ids, quantities, an address and a chosen shipping option; placing an
//! order re-runs the quote, so a tampered client can't change what it pays.

pub mod inventory;
pub mod orders;
pub mod quote;

use axum::http::StatusCode;
use serde::{Deserialize, Serialize};
use serde_json::json;
use uuid::Uuid;

use crate::{error::AppError, money::MoneyError, shipping::ShippingError};

#[derive(Debug, thiserror::Error)]
pub enum CheckoutError {
    #[error("{0}")]
    InvalidInput(String),
    /// The SKU doesn't exist, isn't for sale, or has no price in the order currency.
    #[error("item {sku_id} isn't available in this currency")]
    Unavailable { sku_id: Uuid },
    #[error("item {sku_id} is out of stock ({available} left)")]
    OutOfStock { sku_id: Uuid, available: i32 },
    #[error("we don't ship to that address in this currency")]
    ShippingUnavailable,
    #[error("that shipping option isn't available for this order")]
    UnknownShippingOption,
    #[error("this Idempotency-Key was already used for a different request")]
    IdempotencyMismatch,
    #[error("not found")]
    NotFound,
    #[error("{0}")]
    InvalidState(String),
    #[error("order total is too large")]
    Money(#[from] MoneyError),
    #[error(transparent)]
    Shipping(#[from] ShippingError),
    #[error(transparent)]
    Database(#[from] sqlx::Error),
}

impl From<CheckoutError> for AppError {
    fn from(err: CheckoutError) -> Self {
        let rejected = |status, extra: serde_json::Value, err: &CheckoutError| AppError::Rejected {
            status,
            detail: err.to_string(),
            extra: extra.as_object().cloned().unwrap_or_default(),
        };
        match err {
            CheckoutError::Unavailable { sku_id } => rejected(
                StatusCode::CONFLICT,
                json!({"code": "unavailable", "sku_id": sku_id}),
                &err,
            ),
            CheckoutError::OutOfStock { sku_id, available } => rejected(
                StatusCode::CONFLICT,
                json!({"code": "out_of_stock", "sku_id": sku_id, "available": available}),
                &err,
            ),
            CheckoutError::ShippingUnavailable => rejected(
                StatusCode::UNPROCESSABLE_ENTITY,
                json!({"code": "shipping_unavailable"}),
                &err,
            ),
            CheckoutError::UnknownShippingOption => rejected(
                StatusCode::UNPROCESSABLE_ENTITY,
                json!({"code": "unknown_shipping_option"}),
                &err,
            ),
            CheckoutError::IdempotencyMismatch => rejected(
                StatusCode::UNPROCESSABLE_ENTITY,
                json!({"code": "idempotency_key_reused"}),
                &err,
            ),
            CheckoutError::InvalidInput(msg) => AppError::BadRequest(msg),
            CheckoutError::InvalidState(msg) => AppError::Conflict(msg),
            CheckoutError::NotFound => AppError::NotFound,
            CheckoutError::Money(_) => AppError::BadRequest(err.to_string()),
            CheckoutError::Shipping(e) => AppError::Internal(e.into()),
            CheckoutError::Database(e) => e.into(),
        }
    }
}

/// A full delivery address.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct Address {
    pub name: String,
    pub line1: String,
    #[serde(default)]
    pub line2: String,
    pub city: String,
    #[serde(default)]
    pub state: String,
    pub postcode: String,
    /// ISO 3166-1 alpha-2, e.g. `MY`.
    pub country: String,
    #[serde(default)]
    pub phone: String,
}

impl Address {
    /// Trims everything, uppercases the country, and checks required fields.
    pub fn normalized(&self) -> Result<Address, CheckoutError> {
        let t = |s: &str| s.trim().to_owned();
        let a = Address {
            name: t(&self.name),
            line1: t(&self.line1),
            line2: t(&self.line2),
            city: t(&self.city),
            state: t(&self.state),
            postcode: t(&self.postcode),
            country: self.country.trim().to_ascii_uppercase(),
            phone: t(&self.phone),
        };
        for (field, value) in [
            ("name", &a.name),
            ("line1", &a.line1),
            ("city", &a.city),
            ("postcode", &a.postcode),
        ] {
            if value.is_empty() {
                return Err(CheckoutError::InvalidInput(format!(
                    "shipping_address.{field} is required"
                )));
            }
        }
        let fields = [&a.name, &a.line1, &a.line2, &a.city, &a.state, &a.postcode, &a.phone];
        if fields.iter().any(|f| f.chars().count() > 200) {
            return Err(CheckoutError::InvalidInput(
                "address fields are limited to 200 characters".into(),
            ));
        }
        a.destination().normalized().map_err(CheckoutError::InvalidInput)?;
        Ok(a)
    }

    pub fn destination(&self) -> crate::shipping::Destination {
        crate::shipping::Destination {
            country: self.country.clone(),
            state: self.state.clone(),
            postcode: self.postcode.clone(),
        }
    }
}
