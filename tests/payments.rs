//! Online payments end to end against a fake HitPay, plus staff mark-paid.

mod common;

use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
};

use axum::{
    Form, Json, Router,
    extract::{Path, State},
    http::{HeaderMap, StatusCode},
    routing::{get, post},
};
use common::{admin_token, send, send_with_headers};
use gnerkulfloot::config::{Config, HitpayConfig, PaymentsConfig};
use hmac::{Hmac, Mac};
use serde_json::{Value, json};
use sha2::Sha256;
use sqlx::PgPool;

const SALT: &str = "webhook-salt";

/// A stand-in for HitPay's payment-request API.
#[derive(Clone, Default)]
struct FakeHitpay {
    requests: Arc<Mutex<Vec<HashMap<String, String>>>>,
    /// id → (status, amount as HitPay would send it)
    state: Arc<Mutex<HashMap<String, (String, Value)>>>,
}

impl FakeHitpay {
    async fn start() -> (Self, String) {
        let fake = FakeHitpay::default();
        let app = Router::new()
            .route("/v1/payment-requests", post(create))
            .route("/v1/payment-requests/{id}", get(status))
            .with_state(fake.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        (fake, format!("http://{addr}"))
    }

    fn set(&self, id: &str, status: &str, amount: Value) {
        self.state.lock().unwrap().insert(id.into(), (status.into(), amount));
    }

    fn request_count(&self) -> usize {
        self.requests.lock().unwrap().len()
    }
}

async fn create(
    State(fake): State<FakeHitpay>,
    headers: HeaderMap,
    Form(form): Form<Vec<(String, String)>>,
) -> Result<Json<Value>, StatusCode> {
    if headers.get("x-business-api-key").and_then(|v| v.to_str().ok()) != Some("test-key") {
        return Err(StatusCode::UNAUTHORIZED);
    }
    let form: HashMap<String, String> = form.into_iter().collect();
    let id = format!("pr_{}", fake.request_count() + 1);
    // HitPay echoes amounts as strings and currencies in lowercase.
    fake.set(&id, "pending", Value::from(form["amount"].clone()));
    fake.requests.lock().unwrap().push(form);
    Ok(Json(
        json!({"id": id, "url": format!("https://checkout.test/{id}"), "status": "pending"}),
    ))
}

async fn status(State(fake): State<FakeHitpay>, Path(id): Path<String>) -> Result<Json<Value>, StatusCode> {
    let state = fake.state.lock().unwrap();
    let (status, amount) = state.get(&id).ok_or(StatusCode::NOT_FOUND)?;
    Ok(Json(
        json!({"id": id, "status": status, "amount": amount, "currency": "myr"}),
    ))
}

fn sign(body: &str) -> String {
    let mut mac = Hmac::<Sha256>::new_from_slice(SALT.as_bytes()).unwrap();
    mac.update(body.as_bytes());
    mac.finalize().into_bytes().iter().map(|b| format!("{b:02x}")).collect()
}

struct Shop {
    app: Router,
    db: PgPool,
    admin: String,
    sku: String,
    fake: FakeHitpay,
    config: Config,
}

async fn shop(db: PgPool, stock: i32) -> Shop {
    let (fake, base) = FakeHitpay::start().await;
    shop_with(db, hitpay_config(base), stock, fake).await
}

fn hitpay_config(base: String) -> Config {
    let mut config = common::config();
    config.payments = PaymentsConfig::Hitpay(HitpayConfig {
        api_key: "test-key".into(),
        webhook_salt: SALT.into(),
        sandbox: true,
        payment_methods: vec!["duitnow".into(), "touch_n_go".into()],
        return_url: Some("https://shop.test/orders/{order_id}".into()),
        api_base: Some(base),
    });
    config
}

async fn shop_with(db: PgPool, config: Config, stock: i32, fake: FakeHitpay) -> Shop {
    let app = common::router(db.clone(), config.clone());
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
    let sku = send(
        &app,
        "POST",
        &format!("/v1/admin/products/{pid}/skus"),
        a,
        Some(json!({
            "code": "KL-1", "stock_available": stock, "prices": [{"currency": "MYR", "amount": 1000}]
        })),
    )
    .await;
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
        Some(json!({"name": "Std", "currency": "MYR", "amount": 800})),
    )
    .await;
    Shop {
        sku: sku.json["id"].as_str().unwrap().to_owned(),
        app,
        db,
        admin,
        fake,
        config,
    }
}

/// Places an order for `qty` and returns (order id, access token).
async fn order(s: &Shop, qty: i32) -> (String, String) {
    let res = send(&s.app, "POST", "/v1/orders", None, Some(json!({
        "email": "siti@example.com", "currency": "MYR", "lines": [{"sku_id": s.sku, "quantity": qty}],
        "shipping_address": {"name": "Siti", "line1": "1 Jalan Ampang", "city": "KL", "postcode": "50450", "country": "MY"}
    }))).await;
    assert_eq!(res.status, StatusCode::CREATED, "{:?}", res.json);
    (
        res.json["order"]["id"].as_str().unwrap().into(),
        res.json["access_token"].as_str().unwrap().into(),
    )
}

async fn pay(s: &Shop, id: &str, token: &str) -> common::Res {
    send_with_headers(
        &s.app,
        "POST",
        &format!("/v1/orders/{id}/payment"),
        &[("x-order-token", token)],
        None,
    )
    .await
}

async fn webhook(s: &Shop, body: &str, signature: &str) -> StatusCode {
    let req = axum::http::Request::post("/v1/payments/hitpay/webhook")
        .header("content-type", "application/json")
        .header("hitpay-signature", signature)
        .header("hitpay-event-object", "payment_request")
        .body(axum::body::Body::from(body.to_owned()))
        .unwrap();
    use tower::ServiceExt;
    s.app.clone().oneshot(req).await.unwrap().status()
}

async fn order_status(s: &Shop, id: &str) -> (String, Option<String>, Option<String>) {
    sqlx::query_as("SELECT status, paid_via, review_reason FROM orders WHERE id = $1::uuid")
        .bind(id)
        .fetch_one(&s.db)
        .await
        .unwrap()
}

async fn stock(db: &PgPool) -> i32 {
    sqlx::query_scalar("SELECT stock_available FROM skus")
        .fetch_one(db)
        .await
        .unwrap()
}

#[sqlx::test]
async fn hitpay_payment_marks_the_order_paid_via_signed_webhook(db: PgPool) {
    let s = shop(db, 5).await;
    let (id, token) = order(&s, 2).await;

    let p = pay(&s, &id, &token).await;
    assert_eq!(p.status, StatusCode::CREATED, "{:?}", p.json);
    assert_eq!(p.json["checkout_url"], "https://checkout.test/pr_1");
    assert_eq!(p.json["amount"], json!({"amount": 2800, "currency": "MYR"}));
    {
        let reqs = s.fake.requests.lock().unwrap();
        let r = &reqs[0];
        assert_eq!(r["amount"], "28.00");
        assert_eq!(r["currency"], "MYR");
        assert_eq!(r["reference_number"], id);
        assert_eq!(r["redirect_url"], format!("https://shop.test/orders/{id}"));
        assert!(r["expires_after"].ends_with(" mins"));
    }

    // Pressing "Pay" again reuses the open checkout.
    let again = pay(&s, &id, &token).await;
    assert_eq!(again.json["id"], p.json["id"]);
    assert_eq!(s.fake.request_count(), 1);

    // A forged webhook is refused and changes nothing.
    let body = r#"{"id":"pr_1","status":"completed"}"#;
    assert_eq!(webhook(&s, body, "deadbeef").await, StatusCode::UNAUTHORIZED);
    assert_eq!(order_status(&s, &id).await.0, "pending_payment");

    s.fake.set("pr_1", "completed", json!(28)); // numeric amount this time
    assert_eq!(webhook(&s, body, &sign(body)).await, StatusCode::OK);
    assert_eq!(
        order_status(&s, &id).await,
        ("paid".into(), Some("hitpay".into()), None)
    );
    assert_eq!(stock(&s.db).await, 3, "paid orders keep their stock");

    // Providers retry webhooks; a repeat changes nothing.
    assert_eq!(webhook(&s, body, &sign(body)).await, StatusCode::OK);
    assert_eq!(order_status(&s, &id).await.0, "paid");
    let events: i64 = sqlx::query_scalar("SELECT count(*) FROM payment_events")
        .fetch_one(&s.db)
        .await
        .unwrap();
    assert_eq!(events, 2);

    // Paid orders can't be paid again.
    assert_eq!(pay(&s, &id, &token).await.json["code"], "order_not_payable");

    let admin_view = send(&s.app, "GET", &format!("/v1/admin/orders/{id}"), Some(&s.admin), None).await;
    assert_eq!(admin_view.json["payments"][0]["status"], "succeeded");
    assert_eq!(admin_view.json["paid_via"], "hitpay");
}

#[sqlx::test]
async fn a_genuine_webhook_body_is_not_trusted_over_the_api(db: PgPool) {
    let s = shop(db, 5).await;
    let (id, token) = order(&s, 1).await;
    pay(&s, &id, &token).await;
    // Correctly signed and *claims* completed, but HitPay's API still says pending.
    let body = r#"{"id":"pr_1","status":"completed","amount":"18.00"}"#;
    assert_eq!(webhook(&s, body, &sign(body)).await, StatusCode::OK);
    assert_eq!(order_status(&s, &id).await.0, "pending_payment");
}

#[sqlx::test]
async fn lost_webhooks_are_caught_by_reconciliation(db: PgPool) {
    let s = shop(db, 5).await;
    let (id, token) = order(&s, 1).await;
    pay(&s, &id, &token).await;
    s.fake.set("pr_1", "completed", json!("18.00"));
    sqlx::query("UPDATE payments SET created_at = now() - interval '5 minutes'")
        .execute(&s.db)
        .await
        .unwrap();

    sweep(&s).await;
    assert_eq!(order_status(&s, &id).await.0, "paid");
}

/// Runs one pass of the background sweeps with the shop's config.
async fn sweep(s: &Shop) {
    let state = gnerkulfloot::app::AppState::new(s.db.clone(), s.config.clone()).unwrap();
    gnerkulfloot::worker::run_once(&state).await;
}

/// Lets an order's payment window run out and the sweep release its stock.
async fn lapse(s: &Shop, id: &str) {
    sqlx::query("UPDATE orders SET expires_at = now() - interval '1 minute' WHERE id = $1::uuid")
        .bind(id)
        .execute(&s.db)
        .await
        .unwrap();
    sweep(s).await;
    assert_eq!(order_status(s, id).await.0, "expired");
}

#[sqlx::test]
async fn paid_after_expiry_takes_its_stock_back_if_still_there(db: PgPool) {
    let s = shop(db, 5).await;
    let (id, token) = order(&s, 2).await;
    pay(&s, &id, &token).await;
    lapse(&s, &id).await;
    assert_eq!(stock(&s.db).await, 5);

    s.fake.set("pr_1", "completed", json!("28.00"));
    let body = r#"{"id":"pr_1"}"#;
    assert_eq!(webhook(&s, body, &sign(body)).await, StatusCode::OK);
    assert_eq!(
        order_status(&s, &id).await,
        ("paid".into(), Some("hitpay".into()), None)
    );
    assert_eq!(stock(&s.db).await, 3);
}

#[sqlx::test]
async fn paid_after_expiry_with_stock_gone_is_flagged_for_review(db: PgPool) {
    let s = shop(db, 2).await;
    let (id, token) = order(&s, 2).await;
    pay(&s, &id, &token).await;
    lapse(&s, &id).await;
    order(&s, 2).await; // someone else buys the released stock
    assert_eq!(stock(&s.db).await, 0);

    s.fake.set("pr_1", "completed", json!("28.00"));
    let body = r#"{"id":"pr_1"}"#;
    assert_eq!(webhook(&s, body, &sign(body)).await, StatusCode::OK);
    let (status, _, review) = order_status(&s, &id).await;
    assert_eq!(status, "expired");
    assert!(review.unwrap().contains("sold out"));
    assert_eq!(stock(&s.db).await, 0, "a partial re-hold must not leak");

    let flagged = send(
        &s.app,
        "GET",
        "/v1/admin/orders?needs_review=true",
        Some(&s.admin),
        None,
    )
    .await;
    assert_eq!(flagged.json["items"].as_array().unwrap().len(), 1);
}

#[sqlx::test]
async fn wrong_amount_is_flagged_not_marked_paid(db: PgPool) {
    let s = shop(db, 5).await;
    let (id, token) = order(&s, 1).await;
    pay(&s, &id, &token).await;
    s.fake.set("pr_1", "completed", json!("1.00"));
    let body = r#"{"id":"pr_1"}"#;
    webhook(&s, body, &sign(body)).await;
    let (status, _, review) = order_status(&s, &id).await;
    assert_eq!(status, "pending_payment");
    assert!(review.unwrap().contains("received 1.00 MYR but asked for 18.00 MYR"));
}

#[sqlx::test]
async fn paying_needs_the_order_token_and_a_configured_adapter(db: PgPool) {
    let s = shop(db.clone(), 5).await;
    let (id, _) = order(&s, 1).await;
    let no_token = send(&s.app, "POST", &format!("/v1/orders/{id}/payment"), None, None).await;
    assert_eq!(no_token.status, StatusCode::NOT_FOUND);
    assert_eq!(s.fake.request_count(), 0);

    // A shop without online payments says so clearly.
    let (fake, _) = FakeHitpay::start().await;
    let plain = Shop {
        app: common::router(db, common::config()),
        ..s
    };
    let (id, token) = order(&plain, 1).await;
    let res = pay(&plain, &id, &token).await;
    assert_eq!(res.status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(res.json["code"], "payments_disabled");
    assert_eq!(fake.request_count(), 0);
}

#[sqlx::test]
async fn staff_mark_offline_payments(db: PgPool) {
    let s = shop(db, 5).await;
    let (id, _) = order(&s, 1).await;
    let path = format!("/v1/admin/orders/{id}/mark-paid");

    let customer = send(
        &s.app,
        "POST",
        "/v1/auth/register",
        None,
        Some(json!({"email": "c@shop.test", "password": "correct horse battery"})),
    )
    .await;
    let customer = customer.json["token"].as_str().unwrap();
    assert_eq!(
        send(&s.app, "POST", &path, Some(customer), None).await.status,
        StatusCode::FORBIDDEN
    );

    let res = send(&s.app, "POST", &path, Some(&s.admin), None).await;
    assert_eq!(res.status, StatusCode::OK, "{:?}", res.json);
    assert_eq!(res.json["status"], "paid");
    assert_eq!(res.json["paid_via"], "manual");
    assert_eq!(
        send(&s.app, "POST", &path, Some(&s.admin), None).await.json["code"],
        "order_not_payable"
    );
}
