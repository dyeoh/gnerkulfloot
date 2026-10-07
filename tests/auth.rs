//! First-time setup, login, sessions, roles and rate limiting, end to end.

mod common;

use axum::http::StatusCode;
use common::send;
use gnerkulfloot::{auth::setup, config::Quota};
use serde_json::json;
use sqlx::PgPool;

const PW: &str = "correct horse battery";

/// Runs first-time setup and returns the admin's session token.
async fn setup_admin(app: &axum::Router, db: &PgPool) -> String {
    let token = setup::token(db, &Default::default()).await.unwrap().unwrap();
    let res = send(
        app,
        "POST",
        "/v1/setup",
        None,
        Some(json!({"token": token, "email": "Admin@Shop.test", "password": PW})),
    )
    .await;
    assert_eq!(res.status, StatusCode::CREATED, "{:?}", res.json);
    res.json["token"].as_str().unwrap().to_owned()
}

#[sqlx::test]
async fn setup_works_exactly_once(db: PgPool) {
    let app = common::router(db.clone(), common::config());

    let res = send(&app, "GET", "/v1/setup", None, None).await;
    assert_eq!(res.json, json!({"required": true}));

    let res = send(
        &app,
        "POST",
        "/v1/setup",
        None,
        Some(json!({"token": "wrong", "email": "a@shop.test", "password": PW})),
    )
    .await;
    assert_eq!(res.status, StatusCode::FORBIDDEN);

    let admin = setup_admin(&app, &db).await;
    let me = send(&app, "GET", "/v1/auth/me", Some(&admin), None).await;
    assert_eq!(me.json["role"], "admin");
    assert_eq!(me.json["email"], "admin@shop.test"); // normalized

    assert_eq!(
        send(&app, "GET", "/v1/setup", None, None).await.json,
        json!({"required": false})
    );
    assert_eq!(setup::token(&db, &Default::default()).await.unwrap(), None);
    let again = send(
        &app,
        "POST",
        "/v1/setup",
        None,
        Some(json!({"token": "anything", "email": "b@shop.test", "password": PW})),
    )
    .await;
    assert_eq!(again.status, StatusCode::CONFLICT);
}

#[sqlx::test]
async fn concurrent_setup_creates_one_admin(db: PgPool) {
    let app = common::router(db.clone(), common::config());
    let token = setup::token(&db, &Default::default()).await.unwrap().unwrap();
    let attempts = (0..5).map(|i| {
        let (app, token) = (app.clone(), token.clone());
        tokio::spawn(async move {
            send(
                &app,
                "POST",
                "/v1/setup",
                None,
                Some(json!({"token": token, "email": format!("a{i}@shop.test"), "password": PW})),
            )
            .await
            .status
        })
    });
    let statuses: Vec<_> = futures_join(attempts).await;
    assert_eq!(
        statuses.iter().filter(|s| **s == StatusCode::CREATED).count(),
        1,
        "{statuses:?}"
    );
    let admins: i64 = sqlx::query_scalar("SELECT count(*) FROM users WHERE role = 'admin'")
        .fetch_one(&db)
        .await
        .unwrap();
    assert_eq!(admins, 1);
}

async fn futures_join<T>(handles: impl Iterator<Item = tokio::task::JoinHandle<T>>) -> Vec<T> {
    let mut out = Vec::new();
    for h in handles.collect::<Vec<_>>() {
        out.push(h.await.unwrap());
    }
    out
}

#[sqlx::test]
async fn customer_register_login_logout(db: PgPool) {
    let app = common::router(db, common::config());
    let creds = json!({"email": "buyer@shop.test", "password": PW});

    let reg = send(&app, "POST", "/v1/auth/register", None, Some(creds.clone())).await;
    assert_eq!(reg.status, StatusCode::CREATED);
    assert_eq!(reg.json["user"]["role"], "customer");
    assert!(reg.json["token"].as_str().unwrap().starts_with("gnk_"));

    let dup = send(&app, "POST", "/v1/auth/register", None, Some(creds.clone())).await;
    assert_eq!(dup.status, StatusCode::CONFLICT);

    let weak = send(
        &app,
        "POST",
        "/v1/auth/register",
        None,
        Some(json!({"email": "x@shop.test", "password": "short"})),
    )
    .await;
    assert_eq!(weak.status, StatusCode::BAD_REQUEST);

    let login = send(&app, "POST", "/v1/auth/login", None, Some(creds)).await;
    assert_eq!(login.status, StatusCode::OK);
    let token = login.json["token"].as_str().unwrap().to_owned();
    assert_eq!(
        send(&app, "GET", "/v1/auth/me", Some(&token), None).await.status,
        StatusCode::OK
    );

    assert_eq!(
        send(&app, "POST", "/v1/auth/logout", Some(&token), None).await.status,
        StatusCode::NO_CONTENT
    );
    let after = send(&app, "GET", "/v1/auth/me", Some(&token), None).await;
    assert_eq!(after.status, StatusCode::UNAUTHORIZED);
    assert_eq!(after.headers["www-authenticate"], "Bearer");
}

#[sqlx::test]
async fn wrong_password_and_unknown_user_look_the_same(db: PgPool) {
    let app = common::router(db, common::config());
    send(
        &app,
        "POST",
        "/v1/auth/register",
        None,
        Some(json!({"email": "buyer@shop.test", "password": PW})),
    )
    .await;
    let wrong = send(
        &app,
        "POST",
        "/v1/auth/login",
        None,
        Some(json!({"email": "buyer@shop.test", "password": "nope nope nope"})),
    )
    .await;
    let unknown = send(
        &app,
        "POST",
        "/v1/auth/login",
        None,
        Some(json!({"email": "ghost@shop.test", "password": PW})),
    )
    .await;
    assert_eq!(wrong.status, StatusCode::UNAUTHORIZED);
    assert_eq!(wrong.json, unknown.json);
}

#[sqlx::test]
async fn account_locks_after_repeated_failures(db: PgPool) {
    let app = common::router(db, common::config()); // max_failed_logins = 5
    send(
        &app,
        "POST",
        "/v1/auth/register",
        None,
        Some(json!({"email": "buyer@shop.test", "password": PW})),
    )
    .await;
    let bad = json!({"email": "buyer@shop.test", "password": "guess guess guess"});
    for _ in 0..5 {
        assert_eq!(
            send(&app, "POST", "/v1/auth/login", None, Some(bad.clone()))
                .await
                .status,
            StatusCode::UNAUTHORIZED
        );
    }
    // Even the right password is refused while locked.
    let locked = send(
        &app,
        "POST",
        "/v1/auth/login",
        None,
        Some(json!({"email": "buyer@shop.test", "password": PW})),
    )
    .await;
    assert_eq!(locked.status, StatusCode::TOO_MANY_REQUESTS);
    assert!(locked.headers.contains_key("retry-after"));
}

#[sqlx::test]
async fn admin_routes_enforce_roles(db: PgPool) {
    let app = common::router(db.clone(), common::config());
    let admin = setup_admin(&app, &db).await;
    let customer = send(
        &app,
        "POST",
        "/v1/auth/register",
        None,
        Some(json!({"email": "buyer@shop.test", "password": PW})),
    )
    .await;
    let customer = customer.json["token"].as_str().unwrap();

    assert_eq!(
        send(&app, "GET", "/v1/admin/users", None, None).await.status,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        send(&app, "GET", "/v1/admin/users", Some(customer), None).await.status,
        StatusCode::FORBIDDEN
    );

    let staff = send(
        &app,
        "POST",
        "/v1/admin/users",
        Some(&admin),
        Some(json!({"email": "helper@shop.test", "password": PW, "role": "staff"})),
    )
    .await;
    assert_eq!(staff.status, StatusCode::CREATED);
    assert_eq!(staff.json["role"], "staff");

    let list = send(&app, "GET", "/v1/admin/users", Some(&admin), None).await;
    let emails: Vec<_> = list
        .json
        .as_array()
        .unwrap()
        .iter()
        .map(|u| u["email"].as_str().unwrap())
        .collect();
    assert_eq!(emails, ["admin@shop.test", "helper@shop.test"]); // customers aren't listed

    // Staff can't manage accounts.
    let staff_login = send(
        &app,
        "POST",
        "/v1/auth/login",
        None,
        Some(json!({"email": "helper@shop.test", "password": PW})),
    )
    .await;
    let staff_token = staff_login.json["token"].as_str().unwrap();
    assert_eq!(
        send(&app, "GET", "/v1/admin/users", Some(staff_token), None)
            .await
            .status,
        StatusCode::FORBIDDEN
    );
}

#[sqlx::test]
async fn login_endpoint_is_rate_limited_per_ip(db: PgPool) {
    let mut config = common::config();
    config.rate_limit.enabled = true;
    config.rate_limit.auth = Quota {
        per_minute: 5,
        burst: 3,
    };
    let app = common::router(db, config);
    let body = json!({"email": "ghost@shop.test", "password": PW});
    for _ in 0..3 {
        assert_eq!(
            send(&app, "POST", "/v1/auth/login", None, Some(body.clone()))
                .await
                .status,
            StatusCode::UNAUTHORIZED
        );
    }
    let limited = send(&app, "POST", "/v1/auth/login", None, Some(body)).await;
    assert_eq!(limited.status, StatusCode::TOO_MANY_REQUESTS);
    assert!(limited.headers["retry-after"].to_str().unwrap().parse::<u64>().unwrap() >= 1);
    // Health checks are never limited.
    assert_eq!(send(&app, "GET", "/healthz", None, None).await.status, StatusCode::OK);
}

#[sqlx::test]
async fn polling_setup_status_does_not_use_login_budget(db: PgPool) {
    let mut config = common::config();
    config.rate_limit.enabled = true;
    config.rate_limit.auth = Quota {
        per_minute: 5,
        burst: 2,
    };
    let app = common::router(db, config);
    for _ in 0..10 {
        assert_eq!(send(&app, "GET", "/v1/setup", None, None).await.status, StatusCode::OK);
    }
    let body = json!({"email": "ghost@shop.test", "password": PW});
    assert_eq!(
        send(&app, "POST", "/v1/auth/login", None, Some(body)).await.status,
        StatusCode::UNAUTHORIZED
    );
}
