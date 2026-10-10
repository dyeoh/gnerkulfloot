//! Checkout end to end: quotes, placing orders, stock holds, idempotency,
//! expiry and cancellation.

mod common;

use axum::{Router, http::StatusCode};
use common::{admin_token, send};
use gnerkulfloot::config::Config;
use serde_json::{Value, json};
use sqlx::PgPool;

struct Shop {
    app: Router,
    db: PgPool,
    admin: String,
    sku: String,
}

/// A shop selling one SKU at RM10.00 (500 g), shipping to Malaysia
/// (RM8, free over RM100; East Malaysia RM15) with 6% SST.
async fn shop(db: PgPool, config: Config, stock: i32) -> Shop {
    let app = common::router(db.clone(), config);
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
            "code": "KL-1", "stock_available": stock, "weight_g": 500,
            "prices": [{"currency": "MYR", "amount": 1000}]
        })),
    )
    .await;
    let sku = sku.json["id"].as_str().unwrap().to_owned();

    let west = send(
        &app,
        "POST",
        "/v1/admin/shipping/zones",
        a,
        Some(json!({"name": "Malaysia", "regions": [{"country": "my"}]})),
    )
    .await;
    assert_eq!(west.status, StatusCode::CREATED, "{:?}", west.json);
    let west = west.json["id"].as_str().unwrap();
    let rate = send(
        &app,
        "POST",
        &format!("/v1/admin/shipping/zones/{west}/rates"),
        a,
        Some(json!({
            "name": "Standard", "currency": "MYR", "amount": 800, "max_weight_g": 5000, "free_over_amount": 10000
        })),
    )
    .await;
    assert_eq!(rate.status, StatusCode::CREATED, "{:?}", rate.json);
    let east = send(&app, "POST", "/v1/admin/shipping/zones", a, Some(json!({
        "name": "East Malaysia", "regions": [{"country": "MY", "state": "Sabah"}, {"country": "MY", "state": "Sarawak"}]
    }))).await;
    let east = east.json["id"].as_str().unwrap();
    send(
        &app,
        "POST",
        &format!("/v1/admin/shipping/zones/{east}/rates"),
        a,
        Some(json!({
            "name": "East Malaysia Standard", "currency": "MYR", "amount": 1500
        })),
    )
    .await;

    let tax = send(
        &app,
        "POST",
        "/v1/admin/tax-rules",
        a,
        Some(json!({"name": "SST", "country": "MY", "rate_bp": 600})),
    )
    .await;
    assert_eq!(tax.status, StatusCode::CREATED, "{:?}", tax.json);
    Shop { app, db, admin, sku }
}

fn address(state: &str) -> Value {
    json!({"name": "Siti", "line1": "1 Jalan Ampang", "city": "Kuala Lumpur", "state": state, "postcode": "50450", "country": "MY"})
}

fn order(sku: &str, qty: i32) -> Value {
    json!({"email": "siti@example.com", "currency": "MYR", "lines": [{"sku_id": sku, "quantity": qty}],
           "shipping_address": address("Selangor")})
}

async fn stock(db: &PgPool) -> i32 {
    sqlx::query_scalar("SELECT stock_available FROM skus")
        .fetch_one(db)
        .await
        .unwrap()
}

async fn place(s: &Shop, body: Value, key: Option<&str>) -> common::Res {
    match key {
        None => send(&s.app, "POST", "/v1/orders", None, Some(body)).await,
        Some(k) => common::send_with_headers(&s.app, "POST", "/v1/orders", &[("idempotency-key", k)], Some(body)).await,
    }
}

#[sqlx::test]
async fn quotes_price_lines_shipping_and_tax(db: PgPool) {
    let s = shop(db, common::config(), 50).await;
    let quote = |state: &str, qty: i32| {
        let body = json!({"currency": "MYR", "lines": [{"sku_id": s.sku, "quantity": qty}],
                          "destination": {"country": "MY", "state": state, "postcode": "50450"}});
        send(&s.app, "POST", "/v1/checkout/quote", None, Some(body))
    };

    let q = quote("Selangor", 2).await;
    assert_eq!(q.status, StatusCode::OK, "{:?}", q.json);
    assert_eq!(q.json["subtotal"]["amount"], 2000);
    assert_eq!(q.json["shipping"]["name"], "Standard");
    assert_eq!(q.json["shipping"]["price"]["amount"], 800);
    assert_eq!(
        q.json["tax"],
        json!({"name": "SST", "rate_bp": 600, "prices_include_tax": false, "amount": {"amount": 120, "currency": "MYR"}})
    );
    assert_eq!(q.json["total"]["amount"], 2920);
    assert_eq!(q.json["lines"][0]["in_stock"], true);

    // A state-level zone overrides the country-wide one.
    let sabah = quote("sabah", 2).await;
    assert_eq!(sabah.json["shipping_options"].as_array().unwrap().len(), 1);
    assert_eq!(sabah.json["shipping"]["price"]["amount"], 1500);

    // Free shipping from RM100.
    assert_eq!(quote("Selangor", 10).await.json["shipping"]["price"]["amount"], 0);
    // Over the 5 kg limit there's no West Malaysia rate.
    assert_eq!(quote("Selangor", 11).await.json["shipping"], Value::Null);
    // Not enough stock is reported, not an error: stock is only held at order time.
    assert_eq!(quote("Selangor", 60).await.json["lines"][0]["in_stock"], false);

    let sg = send(
        &s.app,
        "POST",
        "/v1/checkout/quote",
        None,
        Some(json!({
            "currency": "MYR", "lines": [{"sku_id": s.sku, "quantity": 1}], "destination": {"country": "SG"}
        })),
    )
    .await;
    assert_eq!(sg.json["shipping"], Value::Null);
    assert_eq!(sg.json["tax"]["rate_bp"], 0, "no rule for SG: default rate");

    // A postcode-prefix rule beats the country rule (e.g. a duty-free island).
    send(
        &s.app,
        "POST",
        "/v1/admin/tax-rules",
        Some(&s.admin),
        Some(json!({
            "name": "Duty free", "country": "MY", "postcode_prefix": "87", "rate_bp": 0
        })),
    )
    .await;
    let labuan = send(
        &s.app,
        "POST",
        "/v1/checkout/quote",
        None,
        Some(json!({
            "currency": "MYR", "lines": [{"sku_id": s.sku, "quantity": 1}],
            "destination": {"country": "MY", "state": "Labuan", "postcode": "87000"}
        })),
    )
    .await;
    assert_eq!(labuan.json["tax"]["name"], "Duty free");
    assert_eq!(labuan.json["total"]["amount"], 1800);
}

#[sqlx::test]
async fn tax_inclusive_prices_dont_raise_the_total(db: PgPool) {
    let mut config = common::config();
    config.tax.prices_include_tax = true;
    let s = shop(db, config, 5).await;
    let q = send(&s.app, "POST", "/v1/checkout/quote", None, Some(json!({
        "currency": "MYR", "lines": [{"sku_id": s.sku, "quantity": 1}], "destination": {"country": "MY", "state": "Selangor"}
    }))).await;
    // RM10.00 already includes 6%: 1000 - 1000/1.06 = 56.6 → 57 sen.
    assert_eq!(q.json["tax"]["amount"]["amount"], 57);
    assert_eq!(q.json["total"]["amount"], 1800);
}

#[sqlx::test]
async fn placing_an_order_holds_stock_and_guests_view_it_with_their_token(db: PgPool) {
    let s = shop(db, common::config(), 5).await;
    let res = place(&s, order(&s.sku, 2), None).await;
    assert_eq!(res.status, StatusCode::CREATED, "{:?}", res.json);
    let o = &res.json["order"];
    assert_eq!(o["status"], "pending_payment");
    assert_eq!(o["total"], json!({"amount": 2920, "currency": "MYR"}));
    assert_eq!(o["lines"][0]["product_name"], "Kuih Lapis");
    assert!(o["number"].as_i64().unwrap() >= 1001);
    assert_eq!(stock(&s.db).await, 3);

    let id = o["id"].as_str().unwrap();
    let token = res.json["access_token"].as_str().unwrap();
    let path = format!("/v1/orders/{id}");
    assert_eq!(
        common::send_with_headers(&s.app, "GET", &path, &[("x-order-token", token)], None)
            .await
            .status,
        StatusCode::OK
    );
    assert_eq!(
        send(&s.app, "GET", &format!("{path}?token={token}"), None, None)
            .await
            .status,
        StatusCode::OK
    );
    assert_eq!(
        send(&s.app, "GET", &path, None, None).await.status,
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        send(&s.app, "GET", &format!("{path}?token=ord_wrong"), None, None)
            .await
            .status,
        StatusCode::NOT_FOUND
    );
}

#[sqlx::test]
async fn customers_see_their_own_orders_only(db: PgPool) {
    let s = shop(db, common::config(), 5).await;
    let reg = |email: &str| {
        send(
            &s.app,
            "POST",
            "/v1/auth/register",
            None,
            Some(json!({"email": email, "password": "correct horse battery"})),
        )
    };
    let alice = reg("alice@example.com").await.json["token"]
        .as_str()
        .unwrap()
        .to_owned();
    let bob = reg("bob@example.com").await.json["token"].as_str().unwrap().to_owned();

    let mine = send(&s.app, "POST", "/v1/orders", Some(&alice), Some(order(&s.sku, 1))).await;
    let id = mine.json["order"]["id"].as_str().unwrap();
    assert_eq!(
        send(&s.app, "GET", &format!("/v1/orders/{id}"), Some(&alice), None)
            .await
            .status,
        StatusCode::OK
    );
    assert_eq!(
        send(&s.app, "GET", &format!("/v1/orders/{id}"), Some(&bob), None)
            .await
            .status,
        StatusCode::NOT_FOUND
    );

    let list = send(&s.app, "GET", "/v1/me/orders", Some(&alice), None).await;
    assert_eq!(list.json["items"].as_array().unwrap().len(), 1);
    assert_eq!(list.json["items"][0]["total"]["amount"], 1860);
    assert!(
        send(&s.app, "GET", "/v1/me/orders", Some(&bob), None).await.json["items"]
            .as_array()
            .unwrap()
            .is_empty()
    );

    // A bad token is an error, not a silent guest checkout.
    assert_eq!(
        send(&s.app, "POST", "/v1/orders", Some("gnk_bogus"), Some(order(&s.sku, 1)))
            .await
            .status,
        StatusCode::UNAUTHORIZED
    );
}

#[sqlx::test]
async fn the_last_item_goes_to_exactly_one_order(db: PgPool) {
    let s = shop(db, common::config(), 1).await;
    let tasks: Vec<_> = (0..20)
        .map(|_| {
            let (app, body) = (s.app.clone(), order(&s.sku, 1));
            tokio::spawn(async move { send(&app, "POST", "/v1/orders", None, Some(body)).await })
        })
        .collect();
    let mut created = 0;
    for t in tasks {
        let res = t.await.unwrap();
        match res.status {
            StatusCode::CREATED => created += 1,
            StatusCode::CONFLICT => {
                assert_eq!(res.json["code"], "out_of_stock");
                assert_eq!(res.json["sku_id"], s.sku.as_str());
                assert_eq!(res.json["available"], 0);
            }
            other => panic!("unexpected {other}: {:?}", res.json),
        }
    }
    assert_eq!(created, 1);
    assert_eq!(stock(&s.db).await, 0);
    let orders: i64 = sqlx::query_scalar("SELECT count(*) FROM orders")
        .fetch_one(&s.db)
        .await
        .unwrap();
    assert_eq!(orders, 1);
}

#[sqlx::test]
async fn idempotency_keys_prevent_double_orders(db: PgPool) {
    let s = shop(db, common::config(), 10).await;
    let first = place(&s, order(&s.sku, 1), Some("checkout-123")).await;
    assert_eq!(first.status, StatusCode::CREATED);
    assert!(!first.headers.contains_key("idempotent-replayed"));

    let retry = place(&s, order(&s.sku, 1), Some("checkout-123")).await;
    assert_eq!(retry.status, StatusCode::CREATED);
    assert_eq!(retry.headers["idempotent-replayed"], "true");
    assert_eq!(retry.json, first.json, "same order, same access token");
    assert_eq!(stock(&s.db).await, 9);

    let reused = place(&s, order(&s.sku, 2), Some("checkout-123")).await;
    assert_eq!(reused.status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(reused.json["code"], "idempotency_key_reused");

    // Five simultaneous submits of one checkout: one order.
    let tasks: Vec<_> = (0..5)
        .map(|_| {
            let (app, body) = (s.app.clone(), order(&s.sku, 1));
            tokio::spawn(async move {
                common::send_with_headers(
                    &app,
                    "POST",
                    "/v1/orders",
                    &[("idempotency-key", "double-click")],
                    Some(body),
                )
                .await
            })
        })
        .collect();
    let mut ids = Vec::new();
    for t in tasks {
        let res = t.await.unwrap();
        assert_eq!(res.status, StatusCode::CREATED, "{:?}", res.json);
        ids.push(res.json["order"]["id"].as_str().unwrap().to_owned());
    }
    ids.dedup();
    assert_eq!(ids.len(), 1);
    assert_eq!(stock(&s.db).await, 8);
}

#[sqlx::test]
async fn unpaid_orders_expire_and_release_stock_once(db: PgPool) {
    let mut config = common::config();
    config.checkout.payment_window_minutes = 0;
    let s = shop(db, config.clone(), 5).await;
    let res = place(&s, order(&s.sku, 3), None).await;
    let id = res.json["order"]["id"].as_str().unwrap().to_owned();
    assert_eq!(stock(&s.db).await, 2);

    let state = gnerkulfloot::state::AppState::new(s.db.clone(), config).unwrap();
    gnerkulfloot::worker::run_once(&state).await;
    assert_eq!(stock(&s.db).await, 5);
    let status: String = sqlx::query_scalar("SELECT status FROM orders WHERE id = $1::uuid")
        .bind(&id)
        .fetch_one(&s.db)
        .await
        .unwrap();
    assert_eq!(status, "expired");

    gnerkulfloot::worker::run_once(&state).await;
    assert_eq!(stock(&s.db).await, 5, "a second sweep must not release again");
}

#[sqlx::test]
async fn staff_cancel_unpaid_orders(db: PgPool) {
    let s = shop(db, common::config(), 5).await;
    let res = place(&s, order(&s.sku, 2), None).await;
    let id = res.json["order"]["id"].as_str().unwrap();
    let cancel = format!("/v1/admin/orders/{id}/cancel");

    let done = send(&s.app, "POST", &cancel, Some(&s.admin), None).await;
    assert_eq!(done.json["status"], "cancelled");
    assert_eq!(stock(&s.db).await, 5);
    assert_eq!(
        send(&s.app, "POST", &cancel, Some(&s.admin), None).await.status,
        StatusCode::CONFLICT
    );
    assert_eq!(stock(&s.db).await, 5);

    let list = send(&s.app, "GET", "/v1/admin/orders?status=cancelled", Some(&s.admin), None).await;
    assert_eq!(list.json["items"].as_array().unwrap().len(), 1);
    let by_email = send(&s.app, "GET", "/v1/admin/orders?q=SITI@", Some(&s.admin), None).await;
    assert_eq!(by_email.json["items"].as_array().unwrap().len(), 1);
}

#[sqlx::test]
async fn staff_mark_paid_orders_sent(db: PgPool) {
    let s = shop(db, common::config(), 5).await;
    let res = place(&s, order(&s.sku, 1), None).await;
    let id = res.json["order"]["id"].as_str().unwrap();
    let fulfil = format!("/v1/admin/orders/{id}/fulfil");

    let unpaid = send(&s.app, "POST", &fulfil, Some(&s.admin), None).await;
    assert_eq!(unpaid.status, StatusCode::CONFLICT);

    let paid = format!("/v1/admin/orders/{id}/mark-paid");
    assert_eq!(
        send(&s.app, "POST", &paid, Some(&s.admin), None).await.status,
        StatusCode::OK
    );

    let sent = send(&s.app, "POST", &fulfil, Some(&s.admin), None).await;
    assert_eq!(sent.status, StatusCode::OK);
    assert_eq!(sent.json["status"], "fulfilled");
    assert!(sent.json["fulfilled_at"].is_string());
    assert_eq!(stock(&s.db).await, 4, "sending doesn't touch stock");
    assert_eq!(
        send(&s.app, "POST", &fulfil, Some(&s.admin), None).await.status,
        StatusCode::CONFLICT
    );
}

#[sqlx::test]
async fn orders_are_priced_by_the_server_and_validated(db: PgPool) {
    let s = shop(db, common::config(), 5).await;

    // Extra fields like a client-side "total" are ignored; the server prices everything.
    let mut body = order(&s.sku, 1);
    body["total"] = json!({"amount": 1, "currency": "MYR"});
    body["lines"][0]["unit_price"] = json!(1);
    let res = place(&s, body, None).await;
    assert_eq!(res.json["order"]["total"]["amount"], 1860);

    let mut bad = order(&s.sku, 1);
    bad["shipping_option_id"] = json!("flat_rate:00000000-0000-0000-0000-000000000000");
    let res = place(&s, bad, None).await;
    assert_eq!(res.status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(res.json["code"], "unknown_shipping_option");

    let mut sg = order(&s.sku, 1);
    sg["shipping_address"]["country"] = json!("SG");
    assert_eq!(place(&s, sg, None).await.json["code"], "shipping_unavailable");

    let mut no_city = order(&s.sku, 1);
    no_city["shipping_address"]["city"] = json!(" ");
    assert_eq!(place(&s, no_city, None).await.status, StatusCode::BAD_REQUEST);

    // Not sold in USD.
    let mut usd = order(&s.sku, 1);
    usd["currency"] = json!("USD");
    let res = place(&s, usd, None).await;
    assert_eq!(res.status, StatusCode::CONFLICT);
    assert_eq!(res.json["code"], "unavailable");

    // Drafts can't be bought.
    let pid: uuid::Uuid = sqlx::query_scalar("SELECT product_id FROM skus")
        .fetch_one(&s.db)
        .await
        .unwrap();
    send(
        &s.app,
        "PATCH",
        &format!("/v1/admin/products/{pid}"),
        Some(&s.admin),
        Some(json!({"status": "draft"})),
    )
    .await;
    assert_eq!(place(&s, order(&s.sku, 1), None).await.json["code"], "unavailable");
    assert_eq!(stock(&s.db).await, 4, "only the first order held stock");
}
