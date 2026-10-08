//! Shared helpers for integration tests: build the real router over a test
//! database and fire JSON requests at it.

#![allow(dead_code)] // each test file uses a different subset

use std::net::SocketAddr;

use axum::{
    Router,
    body::Body,
    extract::connect_info::MockConnectInfo,
    http::{Request, StatusCode, header},
    response::Response,
};
use gnerkulfloot::{
    app::{self, AppState},
    config::{
        AuthConfig, Config, DatabaseConfig, RateLimitConfig, ServerConfig, SetupConfig, ShopConfig, StorageConfig,
    },
};
use http_body_util::BodyExt;
use serde_json::Value;
use sqlx::PgPool;
use tower::ServiceExt;

pub fn config() -> Config {
    Config {
        server: ServerConfig::default(),
        database: DatabaseConfig {
            url: String::new(),
            max_connections: 1,
        },
        shop: ShopConfig::default(),
        auth: AuthConfig::default(),
        setup: SetupConfig::default(),
        rate_limit: RateLimitConfig {
            enabled: false,
            ..Default::default()
        },
        // A fresh media directory per test, so tests never see each other's files.
        checkout: Default::default(),
        tax: Default::default(),
        shipping: Default::default(),
        payments: Default::default(),
        storage: StorageConfig::Local {
            path: std::env::temp_dir().join(format!("gnk-test-media-{}", uuid::Uuid::new_v4())),
            public_base_url: "/media".into(),
        },
    }
}

pub fn router(db: PgPool, config: Config) -> Router {
    // Every request appears to come from the same client IP.
    app::router(AppState::new(db, config).unwrap()).layer(MockConnectInfo(SocketAddr::from(([203, 0, 113, 7], 4000))))
}

pub struct Res {
    pub status: StatusCode,
    pub headers: axum::http::HeaderMap,
    pub json: Value,
}

/// Sends raw bytes, e.g. an image upload.
pub async fn send_bytes(app: &Router, path: &str, token: &str, content_type: &str, body: Vec<u8>) -> Res {
    let req = Request::builder()
        .method("POST")
        .uri(path)
        .header(header::AUTHORIZATION, format!("Bearer {token}"))
        .header(header::CONTENT_TYPE, content_type)
        .body(Body::from(body))
        .unwrap();
    into_res(app.clone().oneshot(req).await.unwrap()).await
}

/// Creates the admin through first-time setup and returns their session token.
pub async fn admin_token(app: &Router, db: &PgPool) -> String {
    let token = gnerkulfloot::auth::setup::token(db, &Default::default())
        .await
        .unwrap()
        .unwrap();
    let body = serde_json::json!({"token": token, "email": "admin@shop.test", "password": "correct horse battery"});
    let res = send(app, "POST", "/v1/setup", None, Some(body)).await;
    assert_eq!(res.status, StatusCode::CREATED, "{:?}", res.json);
    res.json["token"].as_str().unwrap().to_owned()
}

/// Like `send`, with extra request headers.
pub async fn send_with_headers(
    app: &Router,
    method: &str,
    path: &str,
    headers: &[(&str, &str)],
    body: Option<Value>,
) -> Res {
    let mut req = Request::builder().method(method).uri(path);
    for (k, v) in headers {
        req = req.header(*k, *v);
    }
    let req = match body {
        Some(b) => req
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(b.to_string())),
        None => req.body(Body::empty()),
    }
    .unwrap();
    into_res(app.clone().oneshot(req).await.unwrap()).await
}

pub async fn send(app: &Router, method: &str, path: &str, token: Option<&str>, body: Option<Value>) -> Res {
    let mut req = Request::builder().method(method).uri(path);
    if let Some(t) = token {
        req = req.header(header::AUTHORIZATION, format!("Bearer {t}"));
    }
    let req = match body {
        Some(b) => req
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(b.to_string())),
        None => req.body(Body::empty()),
    }
    .unwrap();
    into_res(app.clone().oneshot(req).await.unwrap()).await
}

async fn into_res(res: Response) -> Res {
    let status = res.status();
    let headers = res.headers().clone();
    let bytes = res.into_body().collect().await.unwrap().to_bytes();
    let json = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    Res { status, headers, json }
}
