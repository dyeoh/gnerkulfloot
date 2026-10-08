//! Pricing a basket: line prices, shipping options, tax and the total.
//! Quoting is read-only; it never holds stock.

use std::collections::HashMap;

use iso_currency::Currency;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use super::CheckoutError;
use crate::{
    app::AppState,
    money::Money,
    shipping::{self, Destination, ShipmentRequest, ShippingOption},
    tax::{self, TaxRate},
};

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct LineInput {
    pub sku_id: Uuid,
    pub quantity: i32,
}

#[derive(Deserialize)]
pub struct QuoteRequest {
    pub currency: Option<Currency>,
    pub lines: Vec<LineInput>,
    pub destination: Destination,
    /// Picks a shipping option from a previous quote; the cheapest otherwise.
    pub shipping_option_id: Option<String>,
}

#[derive(Clone, Debug, Serialize)]
pub struct Quote {
    pub currency: Currency,
    pub lines: Vec<QuoteLine>,
    pub subtotal: Money,
    pub weight_g: i64,
    pub shipping_options: Vec<ShippingOption>,
    /// The chosen (or cheapest) option. `None` when nothing ships there.
    pub shipping: Option<ShippingOption>,
    pub tax: TaxSummary,
    pub total: Money,
}

#[derive(Clone, Debug, Serialize)]
pub struct QuoteLine {
    pub sku_id: Uuid,
    pub product_id: Uuid,
    pub product_name: String,
    pub sku_code: String,
    pub sku_name: String,
    pub quantity: i32,
    pub unit_price: Money,
    pub subtotal: Money,
    pub tax: Money,
    /// Whether this quantity is in stock right now. Not a promise: stock is
    /// only held once the order is placed.
    pub in_stock: bool,
}

#[derive(Clone, Debug, Serialize)]
pub struct TaxSummary {
    pub name: String,
    pub rate_bp: i32,
    pub prices_include_tax: bool,
    /// Tax on the lines plus, where the rule says so, on shipping. Already
    /// part of the prices when `prices_include_tax` is true.
    pub amount: Money,
    #[serde(skip)]
    pub applies_to_shipping: bool,
}

/// Merges repeated SKUs and checks quantities, keeping first-seen order.
pub(crate) fn merge_lines(
    lines: &[LineInput],
    max_lines: usize,
    max_qty: i32,
) -> Result<Vec<LineInput>, CheckoutError> {
    if lines.is_empty() {
        return Err(CheckoutError::InvalidInput("add at least one item".into()));
    }
    let mut merged: Vec<LineInput> = Vec::new();
    for line in lines {
        if line.quantity < 1 {
            return Err(CheckoutError::InvalidInput("quantities must be at least 1".into()));
        }
        match merged.iter_mut().find(|m| m.sku_id == line.sku_id) {
            Some(m) => m.quantity = m.quantity.saturating_add(line.quantity),
            None => merged.push(line.clone()),
        }
    }
    if merged.len() > max_lines {
        return Err(CheckoutError::InvalidInput(format!(
            "at most {max_lines} different items per order"
        )));
    }
    if merged.iter().any(|m| m.quantity > max_qty) {
        return Err(CheckoutError::InvalidInput(format!(
            "at most {max_qty} of one item per order"
        )));
    }
    Ok(merged)
}

/// Prices a basket for a destination.
///
/// # Errors
/// `Unavailable` for a SKU that isn't for sale in this currency,
/// `UnknownShippingOption` if the chosen option doesn't apply.
pub async fn quote(state: &AppState, req: QuoteRequest) -> Result<Quote, CheckoutError> {
    let cfg = &state.config;
    let currency = req.currency.unwrap_or(cfg.shop.default_currency);
    let lines = merge_lines(&req.lines, cfg.checkout.max_lines, cfg.checkout.max_quantity)?;
    let destination = req.destination.normalized().map_err(CheckoutError::InvalidInput)?;

    let ids: Vec<Uuid> = lines.iter().map(|l| l.sku_id).collect();
    let rows = sqlx::query!(
        r#"SELECT s.id, s.code, s.name, s.stock_available, s.weight_g, p.id AS product_id, p.name AS product_name,
                  sp.price_amount AS "price_amount?"
           FROM skus s
           JOIN products p ON p.id = s.product_id
           LEFT JOIN sku_prices sp ON sp.sku_id = s.id AND sp.price_currency = $2
           WHERE s.id = ANY($1) AND s.active AND p.status = 'active'"#,
        &ids,
        currency.code()
    )
    .fetch_all(&state.db)
    .await?;
    let by_id: HashMap<Uuid, _> = rows.into_iter().map(|r| (r.id, r)).collect();

    let rate = tax::resolve(&state.db, &cfg.tax, &destination).await?;
    let inclusive = cfg.tax.prices_include_tax;

    let mut quote_lines = Vec::with_capacity(lines.len());
    let mut weight_g: i64 = 0;
    for line in &lines {
        let row = by_id
            .get(&line.sku_id)
            .ok_or(CheckoutError::Unavailable { sku_id: line.sku_id })?;
        let price = row
            .price_amount
            .ok_or(CheckoutError::Unavailable { sku_id: line.sku_id })?;
        let unit_price = Money::new(price, currency);
        let subtotal = unit_price.times(i64::from(line.quantity))?;
        weight_g += i64::from(row.weight_g) * i64::from(line.quantity);
        quote_lines.push(QuoteLine {
            sku_id: row.id,
            product_id: row.product_id,
            product_name: row.product_name.clone(),
            sku_code: row.code.clone(),
            sku_name: row.name.clone(),
            quantity: line.quantity,
            unit_price,
            tax: tax::tax_on(subtotal, rate.rate_bp, inclusive, cfg.tax.rounding)?,
            subtotal,
            in_stock: row.stock_available >= line.quantity,
        });
    }
    let subtotal = Money::sum(currency, quote_lines.iter().map(|l| l.subtotal))?;

    let shipment = ShipmentRequest {
        currency,
        destination,
        weight_g,
        subtotal,
    };
    let shipping_options = shipping::quote_all(&state.shipping, &shipment).await?;
    let shipping = match &req.shipping_option_id {
        Some(id) => Some(
            shipping_options
                .iter()
                .find(|o| &o.id == id)
                .cloned()
                .ok_or(CheckoutError::UnknownShippingOption)?,
        ),
        None => shipping_options.first().cloned(),
    };

    totals(
        currency,
        quote_lines,
        subtotal,
        weight_g,
        shipping_options,
        shipping,
        rate,
        inclusive,
        cfg.tax.rounding,
    )
}

#[allow(clippy::too_many_arguments)]
fn totals(
    currency: Currency,
    lines: Vec<QuoteLine>,
    subtotal: Money,
    weight_g: i64,
    shipping_options: Vec<ShippingOption>,
    shipping: Option<ShippingOption>,
    rate: TaxRate,
    inclusive: bool,
    rounding: crate::money::Rounding,
) -> Result<Quote, CheckoutError> {
    let shipping_price = shipping.as_ref().map_or(Money::zero(currency), |s| s.price);
    let shipping_tax = if rate.applies_to_shipping {
        tax::tax_on(shipping_price, rate.rate_bp, inclusive, rounding)?
    } else {
        Money::zero(currency)
    };
    let tax_amount = Money::sum(currency, lines.iter().map(|l| l.tax))?.checked_add(shipping_tax)?;
    let mut total = subtotal.checked_add(shipping_price)?;
    if !inclusive {
        total = total.checked_add(tax_amount)?;
    }
    Ok(Quote {
        currency,
        lines,
        subtotal,
        weight_g,
        shipping_options,
        shipping,
        tax: TaxSummary {
            name: rate.name,
            rate_bp: rate.rate_bp,
            prices_include_tax: inclusive,
            amount: tax_amount,
            applies_to_shipping: rate.applies_to_shipping,
        },
        total,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn line(q: i32) -> LineInput {
        LineInput {
            sku_id: Uuid::nil(),
            quantity: q,
        }
    }

    #[test]
    fn merges_repeated_skus_and_checks_limits() {
        let other = Uuid::from_u128(7);
        let merged = merge_lines(
            &[
                line(2),
                LineInput {
                    sku_id: other,
                    quantity: 1,
                },
                line(3),
            ],
            10,
            10,
        )
        .unwrap();
        assert_eq!(merged.len(), 2);
        assert_eq!((merged[0].sku_id, merged[0].quantity), (Uuid::nil(), 5));
        assert!(merge_lines(&[], 10, 10).is_err());
        assert!(merge_lines(&[line(0)], 10, 10).is_err());
        assert!(merge_lines(&[line(6), line(6)], 10, 10).is_err());
    }
}
