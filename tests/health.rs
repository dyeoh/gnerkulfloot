//! Health probes answer through the full middleware stack.

mod common;

use axum::http::StatusCode;
use sqlx::PgPool;

#[sqlx::test]
async fn readyz_reports_ready_with_request_id(db: PgPool) {
    let app = common::router(db, common::config());
    let res = common::send(&app, "GET", "/readyz", None, None).await;
    assert_eq!(res.status, StatusCode::OK);
    assert!(res.headers.contains_key("x-request-id"));
}

#[sqlx::test]
async fn readyz_fails_when_database_is_gone(db: PgPool) {
    db.close().await;
    let app = common::router(db, common::config());
    let res = common::send(&app, "GET", "/readyz", None, None).await;
    assert_eq!(res.status, StatusCode::SERVICE_UNAVAILABLE);
}
