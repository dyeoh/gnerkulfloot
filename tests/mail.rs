//! Order emails through the durable job queue.

mod common;

use std::sync::{
    Arc,
    atomic::{AtomicU32, Ordering},
};

use async_trait::async_trait;
use axum::{Router, http::StatusCode};
use common::{admin_token, send};
use gnerkulfloot::{
    mail::{Email, Log, MailAdapter, MailError},
    state::AppState,
    worker,
};
use serde_json::json;
use sqlx::PgPool;

struct Shop {
    app: Router,
    state: AppState,
    admin: String,
    sku: String,
}

async fn shop(db: PgPool, mail: Arc<dyn MailAdapter>, stock: i32) -> Shop {
    let mut config = common::config();
    config.shop.name = "Kuih Shop".into();
    let mut state = AppState::new(db.clone(), config).unwrap();
    state.mail = mail;
    let app = common::router_for(state.clone());
    let admin = admin_token(&app, &db).await;
    let a = Some(admin.as_str());
    let p = send(
        &app,
        "POST",
        "/v1/admin/products",
        a,
        Some(json!({"name": "Kuih Lapis", "status": "active"})),
    )
    .await;
    let pid = p.json["id"].as_str().unwrap();
    let sku = send(&app, "POST", &format!("/v1/admin/products/{pid}/skus"), a, Some(json!({
        "code": "KL-1", "name": "Box of 10", "stock_available": stock, "prices": [{"currency": "MYR", "amount": 1000}]
    }))).await;
    let zone = send(
        &app,
        "POST",
        "/v1/admin/shipping/zones",
        a,
        Some(json!({"name": "MY", "regions": [{"country": "MY"}]})),
    )
    .await;
    let zone = zone.json["id"].as_str().unwrap();
    send(
        &app,
        "POST",
        &format!("/v1/admin/shipping/zones/{zone}/rates"),
        a,
        Some(json!({"name": "Pos Laju", "currency": "MYR", "amount": 800})),
    )
    .await;
    Shop {
        sku: sku.json["id"].as_str().unwrap().to_owned(),
        app,
        state,
        admin,
    }
}

async fn order(s: &Shop, name: &str, qty: i32) -> common::Res {
    send(&s.app, "POST", "/v1/orders", None, Some(json!({
        "email": "siti@example.com", "currency": "MYR", "lines": [{"sku_id": s.sku, "quantity": qty}],
        "shipping_address": {"name": name, "line1": "1 Jalan Ampang", "city": "Kuala Lumpur", "postcode": "50450", "country": "MY"}
    }))).await
}

async fn jobs(db: &PgPool) -> Vec<(String, i32, Option<String>, bool)> {
    sqlx::query_as("SELECT kind, attempts, last_error, completed_at IS NOT NULL FROM jobs ORDER BY created_at")
        .fetch_all(db)
        .await
        .unwrap()
}

#[sqlx::test]
async fn customers_get_order_and_payment_emails(db: PgPool) {
    let log = Log::new("Kuih Shop <orders@shop.test>".into());
    let s = shop(db.clone(), Arc::new(log.clone()), 5).await;

    let placed = order(&s, "Siti <script>alert(1)</script>", 2).await;
    assert_eq!(placed.status, StatusCode::CREATED);
    let id = placed.json["order"]["id"].as_str().unwrap().to_owned();
    let number = placed.json["order"]["number"].as_i64().unwrap();
    assert!(log.outbox().is_empty(), "nothing is sent inside the request");

    worker::run_jobs(&s.state).await;
    let sent = log.outbox();
    assert_eq!(sent.len(), 1);
    let email = &sent[0];
    assert_eq!(email.to, "siti@example.com");
    assert_eq!(email.subject, format!("Order #{number} received · Kuih Shop"));
    assert!(email.text.contains("2 × Kuih Lapis (Box of 10)"), "{}", email.text);
    assert!(email.text.contains("Total    28.00 MYR"), "{}", email.text);
    assert!(email.text.contains("hold your items for 24 hours"), "{}", email.text);
    assert!(!email.html.contains("<script>"), "customer input is escaped in HTML");
    assert!(email.html.contains("&lt;script&gt;"));

    let paid = send(
        &s.app,
        "POST",
        &format!("/v1/admin/orders/{id}/mark-paid"),
        Some(&s.admin),
        None,
    )
    .await;
    assert_eq!(paid.status, StatusCode::OK);
    worker::run_jobs(&s.state).await;
    let sent = log.outbox();
    assert_eq!(sent.len(), 2);
    assert_eq!(
        sent[1].subject,
        format!("Payment received for order #{number} · Kuih Shop")
    );
    assert!(sent[1].text.contains("payment of 28.00 MYR"));

    // Running the queue again sends nothing new.
    worker::run_jobs(&s.state).await;
    assert_eq!(log.outbox().len(), 2);
    assert!(jobs(&db).await.iter().all(|j| j.3), "all jobs completed");
}

#[sqlx::test]
async fn no_email_for_an_order_that_never_happened(db: PgPool) {
    let log = Log::new("shop@shop.test".into());
    let s = shop(db.clone(), Arc::new(log.clone()), 1).await;
    let res = order(&s, "Siti", 5).await; // more than in stock: the transaction rolls back
    assert_eq!(res.status, StatusCode::CONFLICT);
    worker::run_jobs(&s.state).await;
    assert!(log.outbox().is_empty());
    assert!(jobs(&db).await.is_empty());
}

/// Fails the first `failures` sends, then works.
struct Flaky {
    failures: u32,
    calls: AtomicU32,
    inner: Log,
}

#[async_trait]
impl MailAdapter for Flaky {
    fn id(&self) -> &'static str {
        "flaky"
    }

    async fn send(&self, email: &Email) -> Result<(), MailError> {
        if self.calls.fetch_add(1, Ordering::SeqCst) < self.failures {
            return Err(MailError::Transport("421 try again later".into()));
        }
        self.inner.send(email).await
    }
}

#[sqlx::test]
async fn failed_sends_are_retried_with_backoff(db: PgPool) {
    let log = Log::new("shop@shop.test".into());
    let flaky = Arc::new(Flaky {
        failures: 2,
        calls: AtomicU32::new(0),
        inner: log.clone(),
    });
    let s = shop(db.clone(), flaky, 5).await;
    order(&s, "Siti", 1).await;

    worker::run_jobs(&s.state).await;
    let (_, attempts, error, done) = jobs(&db).await.remove(0);
    assert_eq!((attempts, done), (1, false));
    assert!(error.unwrap().contains("421"));
    let delay: f64 = sqlx::query_scalar("SELECT EXTRACT(EPOCH FROM run_at - now())::float8 FROM jobs")
        .fetch_one(&db)
        .await
        .unwrap();
    assert!((20.0..=31.0).contains(&delay), "first retry ~30 s later, got {delay}");

    // Not due yet: nothing happens.
    worker::run_jobs(&s.state).await;
    assert_eq!(jobs(&db).await[0].1, 1);

    for expected_attempts in [2, 3] {
        sqlx::query("UPDATE jobs SET run_at = now()")
            .execute(&db)
            .await
            .unwrap();
        worker::run_jobs(&s.state).await;
        assert_eq!(jobs(&db).await[0].1, expected_attempts);
    }
    assert!(jobs(&db).await[0].3, "third attempt succeeded");
    assert_eq!(log.outbox().len(), 1);
}

#[sqlx::test]
async fn jobs_of_a_crashed_worker_are_picked_up_again(db: PgPool) {
    let log = Log::new("shop@shop.test".into());
    let s = shop(db.clone(), Arc::new(log.clone()), 5).await;
    order(&s, "Siti", 1).await;
    // A worker claimed the job, then died: the lock is still in the future.
    sqlx::query("UPDATE jobs SET locked_until = now() + interval '5 minutes', attempts = 1")
        .execute(&db)
        .await
        .unwrap();
    worker::run_jobs(&s.state).await;
    assert!(log.outbox().is_empty(), "a locked job belongs to its worker");

    sqlx::query("UPDATE jobs SET locked_until = now() - interval '1 second'")
        .execute(&db)
        .await
        .unwrap();
    worker::run_jobs(&s.state).await;
    assert_eq!(log.outbox().len(), 1);
}

#[sqlx::test]
async fn jobs_give_up_after_their_last_attempt(db: PgPool) {
    let log = Log::new("shop@shop.test".into());
    let broken = Arc::new(Flaky {
        failures: u32::MAX,
        calls: AtomicU32::new(0),
        inner: log.clone(),
    });
    let s = shop(db.clone(), broken, 5).await;
    order(&s, "Siti", 1).await;
    sqlx::query("UPDATE jobs SET max_attempts = 2")
        .execute(&db)
        .await
        .unwrap();

    for _ in 0..3 {
        sqlx::query("UPDATE jobs SET run_at = now()")
            .execute(&db)
            .await
            .unwrap();
        worker::run_jobs(&s.state).await;
    }
    let (attempts, failed, error): (i32, bool, Option<String>) =
        sqlx::query_as("SELECT attempts, failed_at IS NOT NULL, last_error FROM jobs")
            .fetch_one(&db)
            .await
            .unwrap();
    assert_eq!(
        (attempts, failed),
        (2, true),
        "stopped after max_attempts and kept for inspection"
    );
    assert!(error.unwrap().contains("421"));
}

#[sqlx::test]
async fn emails_go_out_in_the_order_they_were_queued(db: PgPool) {
    let log = Log::new("shop@shop.test".into());
    let s = shop(db.clone(), Arc::new(log.clone()), 50).await;
    // Several orders placed and paid before the worker runs, so the whole
    // backlog is claimed in one batch.
    for _ in 0..4 {
        let placed = order(&s, "Siti", 1).await;
        let id = placed.json["order"]["id"].as_str().unwrap().to_owned();
        send(
            &s.app,
            "POST",
            &format!("/v1/admin/orders/{id}/mark-paid"),
            Some(&s.admin),
            None,
        )
        .await;
    }
    worker::run_jobs(&s.state).await;
    let subjects: Vec<String> = log.outbox().into_iter().map(|e| e.subject).collect();
    let mut expected = Vec::new();
    for number in 1001..1005 {
        expected.push(format!("Order #{number} received · Kuih Shop"));
        expected.push(format!("Payment received for order #{number} · Kuih Shop"));
    }
    assert_eq!(subjects, expected);
}
