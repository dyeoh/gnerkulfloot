//! A durable job queue in Postgres, for work that must happen but not inside
//! the request, like sending emails.
//!
//! Queue a job with [`enqueue`] inside the same transaction as the change
//! that causes it: if that transaction rolls back, the job never existed; if
//! it commits, the job will run even if the server crashes a moment later.
//!
//! Delivery is at least once. A worker that dies after doing a job but before
//! recording it lets the job's lock lapse, and the job runs again. Jobs must
//! tolerate that (a rare duplicate email is acceptable; a duplicate charge
//! would not be, so payments never run as jobs).

use serde::{Deserialize, Serialize};
use sqlx::{PgConnection, PgPool};
use uuid::Uuid;

use crate::{
    checkout::orders,
    config::{CheckoutConfig, ShopConfig},
    mail::{self, MailAdapter, OrderEmail},
};

/// How long a worker owns a claimed job before others may retry it.
const LOCK_SECS: f64 = 300.0;
const BATCH: i64 = 10;
/// Retry delays grow 30 s, 1 min, 2 min… up to this.
const MAX_BACKOFF_SECS: f64 = 3600.0;

/// Every kind of job, with what it needs to run. Stored as JSON, so changing
/// a variant's fields must stay compatible with jobs already queued.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Job {
    /// Sends an order email to the customer.
    OrderEmail { order_id: Uuid, email: OrderEmail },
}

impl Job {
    fn kind(&self) -> &'static str {
        match self {
            Job::OrderEmail { .. } => "order_email",
        }
    }

    /// Jobs with the same key are only queued once.
    fn dedupe_key(&self) -> Option<String> {
        match self {
            Job::OrderEmail { order_id, email } => Some(format!(
                "order_email:{order_id}:{}",
                serde_json::to_value(email).ok()?.as_str()?
            )),
        }
    }
}

/// Queues a job. Call inside the transaction that makes the job necessary.
/// A job whose dedupe key is already queued (or done) is silently skipped.
pub async fn enqueue(conn: &mut PgConnection, job: &Job) -> Result<(), sqlx::Error> {
    sqlx::query!(
        "INSERT INTO jobs (id, kind, payload, dedupe_key) VALUES ($1, $2, $3, $4)
         ON CONFLICT (dedupe_key) DO NOTHING",
        Uuid::now_v7(),
        job.kind(),
        serde_json::to_value(job).expect("jobs serialize"),
        job.dedupe_key(),
    )
    .execute(conn)
    .await?;
    Ok(())
}

#[derive(Debug, thiserror::Error)]
enum JobError {
    #[error("unreadable job payload: {0}")]
    Payload(#[from] serde_json::Error),
    #[error(transparent)]
    Checkout(#[from] crate::checkout::CheckoutError),
    #[error("template error: {0}")]
    Template(#[from] minijinja::Error),
    #[error(transparent)]
    Mail(#[from] mail::MailError),
    #[error(transparent)]
    Database(#[from] sqlx::Error),
}

/// Claims and runs the jobs that are due. Safe on any number of workers at
/// once: each job is claimed by exactly one. Returns how many jobs ran.
pub async fn run_due(
    db: &PgPool,
    mail: &dyn MailAdapter,
    shop: &ShopConfig,
    checkout: &CheckoutConfig,
) -> Result<usize, sqlx::Error> {
    let claimed = sqlx::query!(
        "UPDATE jobs SET locked_until = now() + make_interval(secs => $1), attempts = attempts + 1
         WHERE id IN (
             SELECT id FROM jobs
             WHERE completed_at IS NULL AND failed_at IS NULL AND run_at <= now()
               AND (locked_until IS NULL OR locked_until < now())
             ORDER BY run_at LIMIT $2
             FOR UPDATE SKIP LOCKED
         )
         RETURNING id, kind, payload, attempts, max_attempts, run_at, created_at",
        LOCK_SECS,
        BATCH
    )
    .fetch_all(db)
    .await?;
    // RETURNING doesn't keep the subquery's order, so restore queue order:
    // a customer must get "order received" before "payment received".
    let mut claimed = claimed;
    claimed.sort_by_key(|j| (j.run_at, j.created_at, j.id));

    for job in &claimed {
        let outcome = match serde_json::from_value::<Job>(job.payload.clone()) {
            Ok(parsed) => run(db, mail, shop, checkout, &parsed).await,
            Err(e) => Err(e.into()),
        };
        match outcome {
            Ok(()) => {
                sqlx::query!(
                    "UPDATE jobs SET completed_at = now(), locked_until = NULL, last_error = NULL WHERE id = $1",
                    job.id
                )
                .execute(db)
                .await?;
            }
            Err(e) if job.attempts >= job.max_attempts => {
                tracing::error!(job_id = %job.id, kind = %job.kind, error = %e, "job failed for good");
                sqlx::query!(
                    "UPDATE jobs SET failed_at = now(), locked_until = NULL, last_error = $2 WHERE id = $1",
                    job.id,
                    e.to_string()
                )
                .execute(db)
                .await?;
            }
            Err(e) => {
                let delay = (30.0 * 2f64.powi(job.attempts - 1)).min(MAX_BACKOFF_SECS);
                tracing::warn!(job_id = %job.id, kind = %job.kind, attempt = job.attempts, retry_in_secs = delay, error = %e, "job failed; will retry");
                sqlx::query!(
                    "UPDATE jobs SET run_at = now() + make_interval(secs => $2), locked_until = NULL, last_error = $3
                     WHERE id = $1",
                    job.id,
                    delay,
                    e.to_string()
                )
                .execute(db)
                .await?;
            }
        }
    }
    Ok(claimed.len())
}

async fn run(
    db: &PgPool,
    mail: &dyn MailAdapter,
    shop: &ShopConfig,
    checkout: &CheckoutConfig,
    job: &Job,
) -> Result<(), JobError> {
    match job {
        Job::OrderEmail { order_id, email } => {
            let order = orders::load(&mut *db.acquire().await?, *order_id).await?;
            let message = mail::render_order_email(*email, &order, &shop.name, checkout.payment_window_minutes)?;
            mail.send(&message).await?;
            tracing::info!(order_id = %order_id, ?email, "order email sent");
            Ok(())
        }
    }
}

/// Deletes finished jobs older than `days`, keeping failed ones for inspection.
pub async fn purge_completed(db: &PgPool, days: i32) -> Result<u64, sqlx::Error> {
    Ok(sqlx::query!(
        "DELETE FROM jobs WHERE completed_at < now() - make_interval(days => $1)",
        days
    )
    .execute(db)
    .await?
    .rows_affected())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn jobs_round_trip_and_dedupe_by_order_and_email() {
        let id = Uuid::from_u128(1);
        let job = Job::OrderEmail {
            order_id: id,
            email: OrderEmail::Paid,
        };
        let json = serde_json::to_value(&job).unwrap();
        assert_eq!(json["kind"], "order_email");
        assert_eq!(serde_json::from_value::<Job>(json).unwrap(), job);
        assert_eq!(job.dedupe_key().unwrap(), format!("order_email:{id}:paid"));
        let placed = Job::OrderEmail {
            order_id: id,
            email: OrderEmail::Placed,
        };
        assert_ne!(placed.dedupe_key(), job.dedupe_key());
    }
}
