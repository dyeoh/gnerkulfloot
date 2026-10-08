//! Periodic housekeeping: expiring unpaid orders, re-checking payments whose
//! webhook may have been lost, and deleting expired sessions and old
//! idempotency keys.
//!
//! Every sweep is idempotent and safe to run on all instances at once, so
//! there's no leader election. The durable job queue (for work that must not
//! be lost, like sending emails) is separate and arrives with email support.

use std::time::Duration;

use crate::{app::AppState, checkout::orders, payments};

const INTERVAL: Duration = Duration::from_secs(30);
/// Idempotency keys only need to outlive client retries.
const IDEMPOTENCY_TTL_HOURS: i32 = 24;

/// Runs the sweeps forever, every 30 seconds.
pub async fn run(state: AppState) {
    let mut tick = tokio::time::interval(INTERVAL);
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        tick.tick().await;
        run_once(&state).await;
    }
}

/// One pass of every sweep. Failures are logged and retried next pass.
pub async fn run_once(state: &AppState) {
    // Before expiring orders, so a payment that landed just in time counts.
    if let Err(e) = payments::reconcile(state).await {
        tracing::error!(error = %e, "reconciling payments failed");
    }
    match orders::expire_due(&state.db).await {
        Ok(0) => {}
        Ok(n) => tracing::info!(expired = n, "expired unpaid orders and released their stock"),
        Err(e) => tracing::error!(error = %e, "expiring orders failed"),
    }
    if let Err(e) = sqlx::query!("DELETE FROM sessions WHERE expires_at < now()")
        .execute(&state.db)
        .await
    {
        tracing::error!(error = %e, "purging expired sessions failed");
    }
    if let Err(e) = sqlx::query!(
        "DELETE FROM idempotency_keys WHERE created_at < now() - make_interval(hours => $1)",
        IDEMPOTENCY_TTL_HOURS
    )
    .execute(&state.db)
    .await
    {
        tracing::error!(error = %e, "purging idempotency keys failed");
    }
}
