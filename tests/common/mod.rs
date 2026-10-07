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
    config::{AuthConfig, Config, DatabaseConfig, RateLimitConfig, ServerConfig, SetupConfig, ShopConfig},
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
    }
}

pub fn router(db: PgPool, config: Config) -> Router {
    // Every request appears to come from the same client IP.
    app::router(AppState::new(db, config)).layer(MockConnectInfo(SocketAddr::from(([203, 0, 113, 7], 4000))))
}

pub struct Res {
    pub status: StatusCode,
    pub headers: axum::http::HeaderMap,
    pub json: Value,
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
