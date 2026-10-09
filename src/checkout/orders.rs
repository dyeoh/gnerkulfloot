//! Placing, viewing, cancelling and expiring orders.

use argon2::password_hash::rand_core::{OsRng, RngCore};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use iso_currency::Currency;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use sqlx::{FromRow, PgConnection, PgPool, Postgres, QueryBuilder};
use subtle::ConstantTimeEq;
use time::{Duration, OffsetDateTime};
use uuid::Uuid;

use super::{
    Address, CheckoutError, Pricing, inventory,
    quote::{self, LineInput, QuoteRequest},
};
use crate::{
    auth::{Role, User, users},
    catalog::storefront::{Page, paging},
    jobs::{self, Job},
    mail::OrderEmail,
    money::Money,
    shipping::ShippingOption,
};

const IDEMPOTENCY_SCOPE: &str = "orders";
/// Expired orders are released in batches so one sweep never holds locks for long.
const EXPIRE_BATCH: i64 = 100;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, sqlx::Type)]
#[sqlx(type_name = "text", rename_all = "snake_case")]
#[serde(rename_all = "snake_case")]
pub enum OrderStatus {
    /// Placed and holding stock until paid or `expires_at`.
    PendingPayment,
    Paid,
    Fulfilled,
    Cancelled,
    /// Not paid in time; its stock went back on sale.
    Expired,
}

/// What a client sends to place an order. Serialized as-is to fingerprint
/// the request for idempotency, so field order matters only to that hash.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct NewOrder {
    pub currency: Option<Currency>,
    pub email: String,
    pub lines: Vec<LineInput>,
    pub shipping_address: Address,
    pub shipping_option_id: Option<String>,
    #[serde(default)]
    pub notes: String,
}

#[derive(Debug, Serialize)]
pub struct OrderView {
    pub id: Uuid,
    pub number: i64,
    pub status: OrderStatus,
    pub email: String,
    pub currency: Currency,
    pub lines: Vec<OrderLineView>,
    pub subtotal: Money,
    pub shipping: ShippingOption,
    pub tax: OrderTax,
    pub total: Money,
    pub shipping_address: Address,
    pub notes: String,
    /// How it was paid: a payment adapter id, or `manual` when staff marked it.
    pub paid_via: Option<String>,
    #[serde(with = "time::serde::rfc3339")]
    pub expires_at: OffsetDateTime,
    #[serde(with = "time::serde::rfc3339::option")]
    pub paid_at: Option<OffsetDateTime>,
    #[serde(with = "time::serde::rfc3339::option")]
    pub cancelled_at: Option<OffsetDateTime>,
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
}

#[derive(Debug, Serialize)]
pub struct OrderLineView {
    pub sku_id: Uuid,
    pub product_id: Uuid,
    pub product_name: String,
    pub sku_code: String,
    pub sku_name: String,
    pub quantity: i32,
    pub unit_price: Money,
    pub subtotal: Money,
    pub tax: Money,
}

#[derive(Debug, Serialize)]
pub struct OrderTax {
    pub name: String,
    pub rate_bp: i32,
    pub prices_include_tax: bool,
    pub amount: Money,
}

/// The response to placing an order. `access_token` lets a guest view the
/// order later; it is only ever shown here.
#[derive(Debug, Serialize)]
pub struct PlacedOrder {
    pub order: OrderView,
    pub access_token: String,
}

/// Whether a placed-order response is new or a replay of an earlier request
/// with the same Idempotency-Key.
pub enum Placement {
    Created(Value),
    Replayed(Value),
}

fn hash_token(token: &str) -> Vec<u8> {
    Sha256::digest(token.as_bytes()).to_vec()
}

fn validate_idempotency_key(key: &str) -> Result<(), CheckoutError> {
    if key.is_empty() || key.len() > 255 || !key.is_ascii() {
        return Err(CheckoutError::InvalidInput(
            "Idempotency-Key must be 1–255 ASCII characters".into(),
        ));
    }
    Ok(())
}

/// Places an order: prices it, holds its stock and records it, all in one
/// transaction.
///
/// With an `idempotency_key`, a repeat of the same request returns the
/// original response instead of placing a second order, even when both
/// arrive at once: the second waits on the first's key row, then replays it.
///
/// # Errors
/// `OutOfStock` / `Unavailable` for a problem line, `ShippingUnavailable` when
/// nothing ships to the address, `IdempotencyMismatch` when a key is reused
/// for a different request.
pub async fn place(
    db: &PgPool,
    pricing: &Pricing<'_>,
    req: NewOrder,
    customer_id: Option<Uuid>,
    idempotency_key: Option<&str>,
) -> Result<Placement, CheckoutError> {
    if let Some(key) = idempotency_key {
        validate_idempotency_key(key)?;
    }
    let request_hash = Sha256::digest(serde_json::to_vec(&req).expect("order request serializes")).to_vec();

    // Fast path for retries: replay before re-pricing, so a retry still gets
    // its original order even if, say, a product was archived since.
    if let Some(key) = idempotency_key
        && let Some(replay) = find_replay(&mut *db.acquire().await?, key, &request_hash).await?
    {
        return Ok(Placement::Replayed(replay));
    }

    let email = users::normalize_email(&req.email).map_err(|e| CheckoutError::InvalidInput(e.to_string()))?;
    let address = req.shipping_address.normalized()?;
    let notes = req.notes.trim().to_owned();
    if notes.chars().count() > 1000 {
        return Err(CheckoutError::InvalidInput(
            "notes are limited to 1000 characters".into(),
        ));
    }

    let quote = quote::quote(
        db,
        pricing,
        QuoteRequest {
            currency: req.currency,
            lines: req.lines.clone(),
            destination: address.destination(),
            shipping_option_id: req.shipping_option_id.clone(),
        },
    )
    .await?;
    let shipping = quote.shipping.clone().ok_or(CheckoutError::ShippingUnavailable)?;

    let mut tx = db.begin().await?;

    if let Some(key) = idempotency_key {
        // If another request with this key is in flight, this insert waits for
        // it to finish, then finds its committed row and replays it.
        let claimed = sqlx::query_scalar!(
            "INSERT INTO idempotency_keys (scope, key, request_hash, response_body)
             VALUES ($1, $2, $3, 'null') ON CONFLICT DO NOTHING RETURNING key",
            IDEMPOTENCY_SCOPE,
            key,
            request_hash,
        )
        .fetch_optional(&mut *tx)
        .await?;
        if claimed.is_none() {
            let replay = find_replay(&mut tx, key, &request_hash).await?;
            return Ok(Placement::Replayed(replay.ok_or(CheckoutError::IdempotencyMismatch)?));
        }
    }

    let lines: Vec<LineInput> = quote
        .lines
        .iter()
        .map(|l| LineInput {
            sku_id: l.sku_id,
            quantity: l.quantity,
        })
        .collect();
    inventory::hold(&mut tx, &lines).await?;

    let mut token_bytes = [0u8; 24];
    OsRng.fill_bytes(&mut token_bytes);
    let access_token = format!("ord_{}", URL_SAFE_NO_PAD.encode(token_bytes));
    let id = Uuid::now_v7();
    let expires_at = OffsetDateTime::now_utc() + Duration::minutes(pricing.checkout.payment_window_minutes);
    let shipping_amount = shipping.price.amount;

    sqlx::query!(
        "INSERT INTO orders (id, status, email, customer_id, currency, subtotal_amount, shipping_amount,
                             tax_amount, total_amount, prices_include_tax, tax_name, tax_rate_bp,
                             shipping_option, shipping_address, notes, access_token_hash, expires_at)
         VALUES ($1, 'pending_payment', $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15, $16)",
        id,
        email,
        customer_id,
        quote.currency.code(),
        quote.subtotal.amount,
        shipping_amount,
        quote.tax.amount.amount,
        quote.total.amount,
        quote.tax.prices_include_tax,
        quote.tax.name,
        quote.tax.rate_bp,
        serde_json::to_value(&shipping).expect("shipping option serializes"),
        serde_json::to_value(&address).expect("address serializes"),
        notes,
        hash_token(&access_token),
        expires_at,
    )
    .execute(&mut *tx)
    .await?;

    for (position, l) in quote.lines.iter().enumerate() {
        sqlx::query!(
            "INSERT INTO order_lines (id, order_id, sku_id, product_id, product_name, sku_code, sku_name,
                                      quantity, unit_amount, subtotal_amount, tax_amount, position)
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12)",
            Uuid::now_v7(),
            id,
            l.sku_id,
            l.product_id,
            l.product_name,
            l.sku_code,
            l.sku_name,
            l.quantity,
            l.unit_price.amount,
            l.subtotal.amount,
            l.tax.amount,
            position as i32,
        )
        .execute(&mut *tx)
        .await?;
    }

    // Queued in this transaction: sent only if the order commits, and not lost
    // if the server stops right after.
    jobs::enqueue(
        &mut tx,
        &Job::OrderEmail {
            order_id: id,
            email: OrderEmail::Placed,
        },
    )
    .await?;

    let order = load(&mut tx, id).await?;
    let body = serde_json::to_value(PlacedOrder { order, access_token }).expect("order serializes");
    if let Some(key) = idempotency_key {
        sqlx::query!(
            "UPDATE idempotency_keys SET response_body = $3 WHERE scope = $1 AND key = $2",
            IDEMPOTENCY_SCOPE,
            key,
            body
        )
        .execute(&mut *tx)
        .await?;
    }
    tx.commit().await?;
    tracing::info!(order_id = %id, total = %quote.total, "order placed");
    Ok(Placement::Created(body))
}

/// The stored response for a completed request with this key, if any.
///
/// # Errors
/// `IdempotencyMismatch` if the key was used for a different request.
async fn find_replay(conn: &mut PgConnection, key: &str, request_hash: &[u8]) -> Result<Option<Value>, CheckoutError> {
    let prior = sqlx::query!(
        "SELECT request_hash, response_body FROM idempotency_keys WHERE scope = $1 AND key = $2",
        IDEMPOTENCY_SCOPE,
        key
    )
    .fetch_optional(conn)
    .await?;
    match prior {
        None => Ok(None),
        Some(p) if p.request_hash != request_hash => Err(CheckoutError::IdempotencyMismatch),
        Some(p) => Ok(Some(p.response_body)),
    }
}

/// Loads an order with its lines.
pub async fn load(conn: &mut PgConnection, id: Uuid) -> Result<OrderView, CheckoutError> {
    let o = sqlx::query!(
        r#"SELECT id, number, status AS "status: OrderStatus", email, currency, subtotal_amount, shipping_amount,
                  tax_amount, total_amount, prices_include_tax, tax_name, tax_rate_bp, shipping_option,
                  shipping_address, notes, paid_via, expires_at, paid_at, cancelled_at, created_at
           FROM orders WHERE id = $1"#,
        id
    )
    .fetch_optional(&mut *conn)
    .await?
    .ok_or(CheckoutError::NotFound)?;
    let currency = Currency::from_code(&o.currency)
        .ok_or_else(|| CheckoutError::InvalidState(format!("order has unknown currency {}", o.currency)))?;
    let m = |amount| Money::new(amount, currency);

    let lines = sqlx::query!(
        "SELECT sku_id, product_id, product_name, sku_code, sku_name, quantity, unit_amount, subtotal_amount, tax_amount
         FROM order_lines WHERE order_id = $1 ORDER BY position",
        id
    )
    .fetch_all(&mut *conn)
    .await?
    .into_iter()
    .map(|l| OrderLineView {
        sku_id: l.sku_id,
        product_id: l.product_id,
        product_name: l.product_name,
        sku_code: l.sku_code,
        sku_name: l.sku_name,
        quantity: l.quantity,
        unit_price: m(l.unit_amount),
        subtotal: m(l.subtotal_amount),
        tax: m(l.tax_amount),
    })
    .collect();

    let corrupt = |what: &str| CheckoutError::InvalidState(format!("order {id} has an unreadable {what}"));
    Ok(OrderView {
        id: o.id,
        number: o.number,
        status: o.status,
        email: o.email,
        currency,
        lines,
        subtotal: m(o.subtotal_amount),
        shipping: serde_json::from_value(o.shipping_option).map_err(|_| corrupt("shipping option"))?,
        tax: OrderTax {
            name: o.tax_name,
            rate_bp: o.tax_rate_bp,
            prices_include_tax: o.prices_include_tax,
            amount: m(o.tax_amount),
        },
        total: m(o.total_amount),
        shipping_address: serde_json::from_value(o.shipping_address).map_err(|_| corrupt("address"))?,
        notes: o.notes,
        paid_via: o.paid_via,
        expires_at: o.expires_at,
        paid_at: o.paid_at,
        cancelled_at: o.cancelled_at,
        created_at: o.created_at,
    })
}

/// Why staff need to look at this order, if they do.
pub async fn review_reason(db: &PgPool, id: Uuid) -> Result<Option<String>, sqlx::Error> {
    Ok(
        sqlx::query_scalar!("SELECT review_reason FROM orders WHERE id = $1", id)
            .fetch_optional(db)
            .await?
            .flatten(),
    )
}

/// Loads an order for someone who presented an access token and/or is logged in.
/// Anyone not allowed to see it gets `NotFound`, so order ids can't be probed.
pub async fn view(db: &PgPool, id: Uuid, token: Option<&str>, user: Option<&User>) -> Result<OrderView, CheckoutError> {
    let access = sqlx::query!("SELECT customer_id, access_token_hash FROM orders WHERE id = $1", id)
        .fetch_optional(db)
        .await?
        .ok_or(CheckoutError::NotFound)?;
    let token_ok = token.is_some_and(|t| bool::from(hash_token(t).ct_eq(&access.access_token_hash)));
    let user_ok = user.is_some_and(|u| matches!(u.role, Role::Admin | Role::Staff) || access.customer_id == Some(u.id));
    if !(token_ok || user_ok) {
        return Err(CheckoutError::NotFound);
    }
    load(&mut *db.acquire().await?, id).await
}

/// Cancels an unpaid order and puts its stock back.
///
/// # Errors
/// `InvalidState` if the order is no longer awaiting payment.
pub async fn cancel(db: &PgPool, id: Uuid) -> Result<OrderView, CheckoutError> {
    let mut tx = db.begin().await?;
    let status = sqlx::query_scalar!(
        r#"SELECT status AS "status: OrderStatus" FROM orders WHERE id = $1 FOR UPDATE"#,
        id
    )
    .fetch_optional(&mut *tx)
    .await?
    .ok_or(CheckoutError::NotFound)?;
    if status != OrderStatus::PendingPayment {
        return Err(CheckoutError::InvalidState(format!(
            "only orders awaiting payment can be cancelled; this one is {}",
            serde_json::to_value(status)
                .expect("status serializes")
                .as_str()
                .unwrap_or("?")
        )));
    }
    inventory::release(&mut tx, &[id]).await?;
    sqlx::query!(
        "UPDATE orders SET status = 'cancelled', cancelled_at = now(), updated_at = now() WHERE id = $1",
        id
    )
    .execute(&mut *tx)
    .await?;
    let order = load(&mut tx, id).await?;
    tx.commit().await?;
    Ok(order)
}

/// Expires unpaid orders past their payment window and puts their stock back.
/// Safe to run on every instance at once: each order is claimed by exactly one
/// sweep (`SKIP LOCKED`). Returns how many orders were expired.
pub async fn expire_due(db: &PgPool) -> Result<u64, sqlx::Error> {
    let mut total = 0;
    loop {
        let mut tx = db.begin().await?;
        let ids = sqlx::query_scalar!(
            "SELECT id FROM orders WHERE status = 'pending_payment' AND expires_at <= now()
             ORDER BY expires_at LIMIT $1 FOR UPDATE SKIP LOCKED",
            EXPIRE_BATCH
        )
        .fetch_all(&mut *tx)
        .await?;
        if ids.is_empty() {
            return Ok(total);
        }
        inventory::release(&mut tx, &ids).await?;
        sqlx::query!(
            "UPDATE orders SET status = 'expired', updated_at = now() WHERE id = ANY($1)",
            &ids
        )
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        total += ids.len() as u64;
        if (ids.len() as i64) < EXPIRE_BATCH {
            return Ok(total);
        }
    }
}

/// One row in an order list.
#[derive(Debug, Serialize, FromRow)]
pub struct OrderSummaryRow {
    pub id: Uuid,
    pub number: i64,
    pub status: OrderStatus,
    pub email: String,
    pub currency: String,
    pub total_amount: i64,
    pub item_count: i64,
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
}

#[derive(Debug, Serialize)]
pub struct OrderSummary {
    pub id: Uuid,
    pub number: i64,
    pub status: OrderStatus,
    pub email: String,
    pub total: Option<Money>,
    pub item_count: i64,
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
}

impl From<OrderSummaryRow> for OrderSummary {
    fn from(r: OrderSummaryRow) -> Self {
        Self {
            total: Currency::from_code(r.currency.trim()).map(|c| Money::new(r.total_amount, c)),
            id: r.id,
            number: r.number,
            status: r.status,
            email: r.email,
            item_count: r.item_count,
            created_at: r.created_at,
        }
    }
}

#[derive(Deserialize, Default)]
pub struct ListQuery {
    pub status: Option<OrderStatus>,
    /// Matches an order number exactly, or part of an email address.
    pub q: Option<String>,
    /// Only orders a person needs to look at (e.g. paid after their stock sold out).
    #[serde(default)]
    pub needs_review: bool,
    pub page: Option<u32>,
    pub per_page: Option<u32>,
}

/// Lists orders newest first, optionally for one customer only.
pub async fn list(db: &PgPool, q: ListQuery, customer_id: Option<Uuid>) -> Result<Page<OrderSummary>, sqlx::Error> {
    let (page, per_page) = paging(q.page, q.per_page);
    let mut sql = QueryBuilder::<Postgres>::new(
        "SELECT o.id, o.number, o.status, o.email, o.currency, o.total_amount, o.created_at,
                (SELECT COALESCE(SUM(quantity), 0) FROM order_lines l WHERE l.order_id = o.id)::bigint AS item_count
         FROM orders o WHERE true",
    );
    if let Some(customer) = customer_id {
        sql.push(" AND o.customer_id = ").push_bind(customer);
    }
    if let Some(status) = q.status {
        sql.push(" AND o.status = ").push_bind(status);
    }
    if q.needs_review {
        sql.push(" AND o.review_reason IS NOT NULL");
    }
    if let Some(term) = q.q.as_deref().map(str::trim).filter(|t| !t.is_empty()) {
        match term.trim_start_matches('#').parse::<i64>() {
            Ok(number) => sql.push(" AND o.number = ").push_bind(number),
            Err(_) => sql
                .push(" AND o.email ILIKE ")
                .push_bind(crate::catalog::storefront::like_pattern(&term.to_lowercase())),
        };
    }
    sql.push(" ORDER BY o.created_at DESC, o.id LIMIT ")
        .push_bind(i64::from(per_page) + 1)
        .push(" OFFSET ")
        .push_bind(i64::from((page - 1) * per_page));
    let rows = sql.build_query_as::<OrderSummaryRow>().fetch_all(db).await?;
    Ok(Page::new(rows.into_iter().map(Into::into).collect(), page, per_page))
}
