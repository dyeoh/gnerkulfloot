//! The background worker: runs queued jobs (see [`crate::jobs`]) every couple
//! of seconds, and periodic housekeeping every 30 seconds: expiring unpaid
//! orders, re-checking payments whose webhook may have been lost, and deleting
//! expired sessions, old idempotency keys and finished jobs.
//!
//! Everything here is safe to run on all instances at once: jobs and sweeps
//! claim their rows with `SKIP LOCKED`, and sweeps are idempotent. So there's
//! no leader election.

use std::time::Duration;

use crate::{checkout::orders, jobs, payments, state::AppState};

const JOB_POLL: Duration = Duration::from_secs(2);
/// Sweeps run every this many job polls (30 s).
const SWEEP_EVERY: u32 = 15;
/// Idempotency keys only need to outlive client retries.
const IDEMPOTENCY_TTL_HOURS: i32 = 24;
const COMPLETED_JOBS_TTL_DAYS: i32 = 14;

/// Runs jobs and sweeps forever.
pub async fn run(state: AppState) {
    let mut tick = tokio::time::interval(JOB_POLL);
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let mut polls: u32 = 0;
    loop {
        tick.tick().await;
        if polls % SWEEP_EVERY == 0 {
            run_once(&state).await;
        } else {
            run_jobs(&state).await;
        }
        polls = polls.wrapping_add(1);
    }
}

/// Runs the jobs that are due, until none are left.
pub async fn run_jobs(state: &AppState) {
    loop {
        match jobs::run_due(state).await {
            Ok(0) => return,
            Ok(_) => continue,
            Err(e) => {
                tracing::error!(error = %e, "running jobs failed");
                return;
            }
        }
    }
}

/// One pass of every sweep, then any due jobs. Failures are logged and
/// retried next pass.
pub async fn run_once(state: &AppState) {
    // Before expiring orders, so a payment that landed just in time counts.
    if let Err(e) = payments::reconcile(&state.db, state.payments.as_deref()).await {
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
    if let Err(e) = jobs::purge_completed(state, COMPLETED_JOBS_TTL_DAYS).await {
        tracing::error!(error = %e, "purging finished jobs failed");
    }
    run_jobs(state).await;
}
