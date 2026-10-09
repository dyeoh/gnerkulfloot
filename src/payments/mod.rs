//! Online payments: the `PaymentAdapter` trait, and what happens when a
//! provider says money arrived.
//!
//! An order is only ever marked paid by (a) the payment provider, confirmed
//! through its authenticated API, or (b) a staff member. Redirects back from a
//! checkout page prove nothing and are never trusted.

mod hitpay;

use std::sync::Arc;

use async_trait::async_trait;
use axum::http::{HeaderMap, StatusCode};
use iso_currency::Currency;
use serde::Serialize;
use serde_json::json;
use sqlx::{PgConnection, PgPool};
use time::OffsetDateTime;
use uuid::Uuid;

pub use hitpay::Hitpay;

use crate::{
    auth::User,
    checkout::{
        CheckoutError, inventory,
        orders::{self, OrderStatus},
        quote::LineInput,
    },
    config::PaymentsConfig,
    error::AppError,
    jobs::{self, Job},
    mail::OrderEmail,
    money::Money,
    state::AppState,
};

/// Pending payments older than this are re-checked with the provider, in case
/// its webhook never arrived.
const RECONCILE_AFTER_SECS: i32 = 120;
const RECONCILE_BATCH: i64 = 20;

/// What we ask a provider to collect.
#[derive(Clone, Debug)]
pub struct PaymentRequest {
    pub order_id: Uuid,
    pub order_number: i64,
    pub amount: Money,
    pub email: String,
    pub name: String,
    /// Minutes until the order expires; the provider's request should expire too.
    pub expires_in_minutes: i64,
}

/// A payment the provider has opened for us.
#[derive(Clone, Debug)]
pub struct CreatedPayment {
    /// The provider's id for it.
    pub provider_ref: String,
    /// Hosted checkout page to send the shopper to.
    pub checkout_url: Option<String>,
    /// A QR payload (e.g. DuitNow EMV string) the storefront can render itself.
    pub qr_code: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RemoteState {
    Pending,
    Succeeded,
    Failed,
}

/// A payment's state according to the provider's own API.
#[derive(Clone, Debug)]
pub struct RemoteStatus {
    pub state: RemoteState,
    pub amount: Money,
}

#[derive(Debug, thiserror::Error)]
pub enum PaymentError {
    #[error("webhook signature is missing or wrong")]
    InvalidSignature,
    #[error("payment provider error: {0}")]
    Provider(String),
    #[error("payment provider unreachable: {0}")]
    Http(#[from] reqwest::Error),
}

#[async_trait]
pub trait PaymentAdapter: Send + Sync {
    /// Short id used in URLs (`/v1/payments/{id}/webhook`) and stored on payments.
    fn id(&self) -> &'static str;

    /// Opens a payment with the provider.
    async fn create(&self, req: &PaymentRequest) -> Result<CreatedPayment, PaymentError>;

    /// Checks a webhook really came from the provider and returns the
    /// provider's id for the payment it concerns (`None` for genuine webhooks
    /// about something else). Never trust the rest of the body: call
    /// `fetch_status` for the facts.
    fn verify_webhook(&self, headers: &HeaderMap, body: &[u8]) -> Result<Option<String>, PaymentError>;

    /// Asks the provider's API for the payment's current state.
    async fn fetch_status(&self, provider_ref: &str, currency: Currency) -> Result<RemoteStatus, PaymentError>;
}

/// Builds the adapter selected in config, if any.
pub fn from_config(cfg: &PaymentsConfig) -> Option<Arc<dyn PaymentAdapter>> {
    match cfg {
        PaymentsConfig::None => None,
        PaymentsConfig::Hitpay(c) => Some(Arc::new(Hitpay::new(c.clone()))),
    }
}

/// Errors from starting or settling payments.
#[derive(Debug, thiserror::Error)]
pub enum PayError {
    #[error("online payments aren't set up for this shop")]
    Disabled,
    #[error("{0}")]
    NotPayable(String),
    #[error(transparent)]
    Provider(#[from] PaymentError),
    #[error(transparent)]
    Checkout(#[from] CheckoutError),
    #[error(transparent)]
    Database(#[from] sqlx::Error),
}

impl From<PayError> for AppError {
    fn from(err: PayError) -> Self {
        let rejected = |status, code: &str, err: &PayError| AppError::Rejected {
            status,
            detail: err.to_string(),
            extra: json!({"code": code}).as_object().cloned().unwrap_or_default(),
        };
        match err {
            PayError::Disabled => rejected(StatusCode::UNPROCESSABLE_ENTITY, "payments_disabled", &err),
            PayError::NotPayable(_) => rejected(StatusCode::CONFLICT, "order_not_payable", &err),
            PayError::Provider(PaymentError::InvalidSignature) => AppError::Unauthorized,
            PayError::Provider(ref e) => {
                tracing::error!(error = %e, "payment provider call failed");
                rejected(StatusCode::BAD_GATEWAY, "payment_provider_error", &err)
            }
            PayError::Checkout(e) => e.into(),
            PayError::Database(e) => e.into(),
        }
    }
}

#[derive(Debug, Serialize)]
pub struct PaymentView {
    pub id: Uuid,
    pub adapter: String,
    pub status: String,
    pub amount: Money,
    pub checkout_url: Option<String>,
    pub qr_code: Option<String>,
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
    #[serde(with = "time::serde::rfc3339::option")]
    pub completed_at: Option<OffsetDateTime>,
}

/// Payments made towards an order, newest first.
pub async fn for_order(db: &PgPool, order_id: Uuid) -> Result<Vec<PaymentView>, sqlx::Error> {
    let rows = sqlx::query!(
        "SELECT id, adapter, status, requested_amount, currency, checkout_url, qr_code, created_at, completed_at
         FROM payments WHERE order_id = $1 ORDER BY created_at DESC",
        order_id
    )
    .fetch_all(db)
    .await?;
    Ok(rows
        .into_iter()
        .filter_map(|r| {
            Some(PaymentView {
                amount: Money::new(r.requested_amount, Currency::from_code(r.currency.trim())?),
                id: r.id,
                adapter: r.adapter,
                status: r.status,
                checkout_url: r.checkout_url,
                qr_code: r.qr_code,
                created_at: r.created_at,
                completed_at: r.completed_at,
            })
        })
        .collect())
}

/// Starts paying for an order, or returns the payment already in progress for
/// it, so a shopper pressing "Pay" twice gets one checkout, not two.
///
/// # Errors
/// `Disabled` without a payment adapter, `NotPayable` once the order is no
/// longer awaiting payment, `NotFound` (via `Checkout`) for callers who can't
/// see the order.
pub async fn start(
    state: &AppState,
    order_id: Uuid,
    token: Option<&str>,
    user: Option<&User>,
) -> Result<PaymentView, PayError> {
    let adapter = state.payments.clone().ok_or(PayError::Disabled)?;
    let order = orders::view(&state.db, order_id, token, user).await?;

    let mut tx = state.db.begin().await?;
    // Lock the order so two "Pay" presses can't open two provider checkouts.
    let row = sqlx::query!(
        r#"SELECT status AS "status: OrderStatus", expires_at FROM orders WHERE id = $1 FOR UPDATE"#,
        order_id
    )
    .fetch_one(&mut *tx)
    .await?;
    let minutes_left = (row.expires_at - OffsetDateTime::now_utc()).whole_minutes();
    if row.status != OrderStatus::PendingPayment || minutes_left < 1 {
        return Err(PayError::NotPayable("this order is no longer awaiting payment".into()));
    }

    let existing = sqlx::query_scalar!(
        "SELECT id FROM payments WHERE order_id = $1 AND adapter = $2 AND status = 'pending'
           AND requested_amount = $3 ORDER BY created_at DESC LIMIT 1",
        order_id,
        adapter.id(),
        order.total.amount
    )
    .fetch_optional(&mut *tx)
    .await?;
    if let Some(id) = existing {
        tx.commit().await?;
        return find(&state.db, id).await;
    }

    let created = adapter
        .create(&PaymentRequest {
            order_id,
            order_number: order.number,
            amount: order.total,
            email: order.email.clone(),
            name: order.shipping_address.name.clone(),
            expires_in_minutes: minutes_left,
        })
        .await?;
    let id = Uuid::now_v7();
    sqlx::query!(
        "INSERT INTO payments (id, order_id, adapter, provider_ref, status, requested_amount, currency, checkout_url, qr_code)
         VALUES ($1, $2, $3, $4, 'pending', $5, $6, $7, $8)",
        id,
        order_id,
        adapter.id(),
        created.provider_ref,
        order.total.amount,
        order.total.currency.code(),
        created.checkout_url,
        created.qr_code,
    )
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    tracing::info!(order_id = %order_id, payment_id = %id, adapter = adapter.id(), "payment started");
    find(&state.db, id).await
}

async fn find(db: &PgPool, payment_id: Uuid) -> Result<PaymentView, PayError> {
    let order_id = sqlx::query_scalar!("SELECT order_id FROM payments WHERE id = $1", payment_id)
        .fetch_one(db)
        .await?;
    for_order(db, order_id)
        .await?
        .into_iter()
        .find(|p| p.id == payment_id)
        .ok_or(PayError::Checkout(CheckoutError::NotFound))
}

/// Handles a provider webhook: checks it's genuine, records it, then asks the
/// provider's API what actually happened and applies that.
///
/// # Errors
/// `InvalidSignature` (→ 401) for forged or corrupted webhooks.
pub async fn handle_webhook(
    state: &AppState,
    adapter_id: &str,
    headers: &HeaderMap,
    body: &[u8],
) -> Result<(), PayError> {
    let adapter = state
        .payments
        .clone()
        .filter(|a| a.id() == adapter_id)
        .ok_or(PayError::Checkout(CheckoutError::NotFound))?;
    let Some(provider_ref) = adapter.verify_webhook(headers, body)? else {
        return Ok(());
    };
    let payment_id = sqlx::query_scalar!(
        "SELECT id FROM payments WHERE adapter = $1 AND provider_ref = $2",
        adapter.id(),
        provider_ref
    )
    .fetch_optional(&state.db)
    .await?;
    let payload = serde_json::from_slice(body).unwrap_or_else(|_| json!({"unparseable": true}));
    sqlx::query!(
        "INSERT INTO payment_events (id, adapter, payment_id, payload) VALUES ($1, $2, $3, $4)",
        Uuid::now_v7(),
        adapter.id(),
        payment_id,
        payload
    )
    .execute(&state.db)
    .await?;
    match payment_id {
        Some(id) => refresh(state, id).await,
        None => {
            tracing::warn!(adapter = adapter.id(), %provider_ref, "webhook for a payment we don't know; ignored");
            Ok(())
        }
    }
}

/// Re-reads a payment's state from its provider and applies it.
pub async fn refresh(state: &AppState, payment_id: Uuid) -> Result<(), PayError> {
    let adapter = state.payments.clone().ok_or(PayError::Disabled)?;
    let p = sqlx::query!(
        "SELECT adapter, provider_ref, currency FROM payments WHERE id = $1",
        payment_id
    )
    .fetch_one(&state.db)
    .await?;
    if p.adapter != adapter.id() {
        return Ok(()); // taken by an adapter that's no longer configured
    }
    let currency = Currency::from_code(p.currency.trim())
        .ok_or_else(|| PayError::NotPayable(format!("unknown currency {}", p.currency)))?;
    let remote = adapter.fetch_status(&p.provider_ref, currency).await?;
    apply(&state.db, payment_id, &remote).await
}

/// Applies a provider's verdict. Idempotent: a payment only leaves `pending`
/// once, so repeated webhooks and reconciliation passes change nothing.
async fn apply(db: &PgPool, payment_id: Uuid, remote: &RemoteStatus) -> Result<(), PayError> {
    let mut tx = db.begin().await?;
    let p = sqlx::query!(
        "SELECT status, order_id, adapter, requested_amount, currency FROM payments WHERE id = $1 FOR UPDATE",
        payment_id
    )
    .fetch_one(&mut *tx)
    .await?;
    if p.status != "pending" {
        return Ok(());
    }
    match remote.state {
        RemoteState::Pending => return Ok(()),
        RemoteState::Failed => {
            sqlx::query!(
                "UPDATE payments SET status = 'failed', updated_at = now() WHERE id = $1",
                payment_id
            )
            .execute(&mut *tx)
            .await?;
        }
        RemoteState::Succeeded => {
            sqlx::query!(
                "UPDATE payments SET status = 'succeeded', completed_at = now(), updated_at = now() WHERE id = $1",
                payment_id
            )
            .execute(&mut *tx)
            .await?;
            let requested = Money::new(p.requested_amount, remote.amount.currency);
            if remote.amount != requested || !p.currency.trim().eq_ignore_ascii_case(remote.amount.currency.code()) {
                // Money arrived, but not what we asked for: a person decides.
                flag(
                    &mut tx,
                    p.order_id,
                    &format!("received {} but asked for {}", remote.amount, requested),
                )
                .await?;
            } else {
                settle(&mut tx, p.order_id, &p.adapter).await?;
            }
            tracing::info!(payment_id = %payment_id, order_id = %p.order_id, "payment succeeded");
        }
    }
    tx.commit().await?;
    Ok(())
}

async fn flag(conn: &mut PgConnection, order_id: Uuid, reason: &str) -> Result<(), sqlx::Error> {
    tracing::warn!(%order_id, reason, "order needs review");
    sqlx::query!(
        "UPDATE orders SET review_reason = $2, updated_at = now() WHERE id = $1",
        order_id,
        reason
    )
    .execute(conn)
    .await?;
    Ok(())
}

/// Marks an order paid now that its money has arrived.
///
/// - Awaiting payment: paid.
/// - Expired or cancelled (its stock was released): try to hold the stock
///   again. If it's still there the order is paid as normal; if not, it's
///   flagged for staff to refund or restock.
/// - Already paid: flagged, since this is a second payment to refund.
async fn settle(conn: &mut PgConnection, order_id: Uuid, via: &str) -> Result<(), PayError> {
    let status = sqlx::query_scalar!(
        r#"SELECT status AS "status: OrderStatus" FROM orders WHERE id = $1 FOR UPDATE"#,
        order_id
    )
    .fetch_one(&mut *conn)
    .await?;
    match status {
        OrderStatus::PendingPayment => {}
        OrderStatus::Expired | OrderStatus::Cancelled => {
            let lines: Vec<LineInput> =
                sqlx::query!("SELECT sku_id, quantity FROM order_lines WHERE order_id = $1", order_id)
                    .fetch_all(&mut *conn)
                    .await?
                    .into_iter()
                    .map(|l| LineInput {
                        sku_id: l.sku_id,
                        quantity: l.quantity,
                    })
                    .collect();
            // A savepoint, so a partial hold (first line held, second sold
            // out) is undone without losing the payment update.
            let mut savepoint = sqlx::Acquire::begin(&mut *conn).await?;
            match inventory::hold(&mut savepoint, &lines).await {
                Ok(()) => savepoint.commit().await?,
                Err(CheckoutError::OutOfStock { .. }) => {
                    savepoint.rollback().await?;
                    flag(
                        conn,
                        order_id,
                        "paid after the order lapsed, and its stock has since sold out: refund or restock",
                    )
                    .await?;
                    return Ok(());
                }
                Err(e) => return Err(e.into()),
            }
        }
        OrderStatus::Paid | OrderStatus::Fulfilled => {
            flag(conn, order_id, "paid more than once: refund the extra payment").await?;
            return Ok(());
        }
    }
    sqlx::query!(
        "UPDATE orders SET status = 'paid', paid_at = now(), paid_via = $2, cancelled_at = NULL, updated_at = now()
         WHERE id = $1",
        order_id,
        via
    )
    .execute(&mut *conn)
    .await?;
    jobs::enqueue(
        conn,
        &Job::OrderEmail {
            order_id,
            email: OrderEmail::Paid,
        },
    )
    .await?;
    Ok(())
}

/// Staff confirm an order was paid outside the shop (cash, bank transfer…).
/// Follows the same rules as an online payment, including re-holding stock
/// for a lapsed order.
pub async fn mark_paid_manually(db: &PgPool, order_id: Uuid) -> Result<(), PayError> {
    let mut tx = db.begin().await?;
    let status = sqlx::query_scalar!(
        r#"SELECT status AS "status: OrderStatus" FROM orders WHERE id = $1"#,
        order_id
    )
    .fetch_optional(&mut *tx)
    .await?
    .ok_or(PayError::Checkout(CheckoutError::NotFound))?;
    if matches!(status, OrderStatus::Paid | OrderStatus::Fulfilled) {
        return Err(PayError::NotPayable("this order is already paid".into()));
    }
    settle(&mut tx, order_id, "manual").await?;
    tx.commit().await?;
    Ok(())
}

/// How long an on-demand check waits before asking the provider about the
/// same payment again, however often the order page polls.
const CHECK_INTERVAL_SECS: i32 = 3;

/// Asks the provider about an order's pending payments right now, for a
/// shopper back from the payment page: they shouldn't wait for the webhook,
/// or for reconciliation's two minutes.
///
/// Safe to expose to shoppers: like a webhook, it only triggers a lookup of
/// the payment's real state through the provider's API; nothing the shopper
/// sends is trusted. Each payment is checked at most once every
/// `CHECK_INTERVAL_SECS`, and the claim is atomic, so polling tabs can't flood
/// the provider. Provider errors are logged and left to reconciliation, so the
/// caller can still show the order.
pub async fn check(state: &AppState, order_id: Uuid) -> Result<(), sqlx::Error> {
    if state.payments.is_none() {
        return Ok(());
    }
    // Touching updated_at is the claim, and also moves the payment to the
    // back of reconciliation's queue.
    let due = sqlx::query_scalar!(
        "UPDATE payments SET updated_at = now()
         WHERE order_id = $1 AND status = 'pending'
           AND updated_at < now() - make_interval(secs => $2)
         RETURNING id",
        order_id,
        f64::from(CHECK_INTERVAL_SECS)
    )
    .fetch_all(&state.db)
    .await?;
    for id in due {
        if let Err(e) = refresh(state, id).await {
            tracing::warn!(payment_id = %id, error = %e, "checking payment failed; reconciliation will retry");
        }
    }
    Ok(())
}

/// Re-checks pending payments whose webhook may have been lost. Returns how
/// many were checked.
pub async fn reconcile(state: &AppState) -> Result<usize, sqlx::Error> {
    if state.payments.is_none() {
        return Ok(0);
    }
    let due = sqlx::query_scalar!(
        "SELECT id FROM payments
         WHERE status = 'pending'
           AND created_at < now() - make_interval(secs => $1)
           AND created_at > now() - interval '3 days'
         ORDER BY updated_at LIMIT $2",
        f64::from(RECONCILE_AFTER_SECS),
        RECONCILE_BATCH
    )
    .fetch_all(&state.db)
    .await?;
    for id in &due {
        // Touch first, so a payment the provider keeps failing on moves to the
        // back of the queue instead of blocking the batch.
        sqlx::query!("UPDATE payments SET updated_at = now() WHERE id = $1", id)
            .execute(&state.db)
            .await?;
        if let Err(e) = refresh(state, *id).await {
            tracing::warn!(payment_id = %id, error = %e, "reconciling payment failed; will retry");
        }
    }
    Ok(due.len())
}
