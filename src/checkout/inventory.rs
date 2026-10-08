//! Holding and releasing stock for orders.
//!
//! Holding is a conditional decrement: it only succeeds if enough stock is
//! left, and Postgres row locks make that atomic across every app instance,
//! so the last unit can only ever go to one order. SKUs are always locked in
//! ascending id order so two multi-item orders can't deadlock on each other.

use std::collections::BTreeMap;

use sqlx::PgConnection;
use uuid::Uuid;

use super::{CheckoutError, quote::LineInput};

/// Takes `lines` out of stock. Call inside the order's transaction: if
/// anything later fails, the rollback puts the stock back.
///
/// # Errors
/// `OutOfStock` naming the first SKU that doesn't have enough.
pub async fn hold(conn: &mut PgConnection, lines: &[LineInput]) -> Result<(), CheckoutError> {
    let by_sku: BTreeMap<Uuid, i32> = lines.iter().map(|l| (l.sku_id, l.quantity)).collect();
    for (sku_id, qty) in by_sku {
        let held = sqlx::query_scalar!(
            "UPDATE skus SET stock_available = stock_available - $2, updated_at = now()
             WHERE id = $1 AND stock_available >= $2
             RETURNING stock_available",
            sku_id,
            qty
        )
        .fetch_optional(&mut *conn)
        .await?;
        if held.is_none() {
            let available = sqlx::query_scalar!("SELECT stock_available FROM skus WHERE id = $1", sku_id)
                .fetch_optional(&mut *conn)
                .await?
                .unwrap_or(0);
            return Err(CheckoutError::OutOfStock { sku_id, available });
        }
    }
    Ok(())
}

/// Puts the stock held by these orders back. The caller must have locked the
/// orders and must change their status in the same transaction, so stock is
/// never returned twice.
pub async fn release(conn: &mut PgConnection, order_ids: &[Uuid]) -> Result<(), sqlx::Error> {
    let totals = sqlx::query!(
        r#"SELECT sku_id, SUM(quantity)::int AS "quantity!" FROM order_lines
           WHERE order_id = ANY($1) GROUP BY sku_id ORDER BY sku_id"#,
        order_ids
    )
    .fetch_all(&mut *conn)
    .await?;
    for t in totals {
        sqlx::query!(
            "UPDATE skus SET stock_available = stock_available + $2, updated_at = now() WHERE id = $1",
            t.sku_id,
            t.quantity
        )
        .execute(&mut *conn)
        .await?;
    }
    Ok(())
}
