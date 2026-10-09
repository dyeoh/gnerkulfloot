//! The OpenAPI spec and the docs page built from it.

mod common;

use axum::http::{StatusCode, header};
use sqlx::PgPool;

#[sqlx::test]
async fn serves_the_spec_and_the_docs_page(db: PgPool) {
    let app = common::router(db, common::config());

    let spec = common::send(&app, "GET", "/openapi.json", None, None).await;
    assert_eq!(spec.status, StatusCode::OK);
    assert!(spec.json["openapi"].as_str().unwrap().starts_with("3."));
    assert_eq!(spec.json["info"]["version"], env!("CARGO_PKG_VERSION"));
    assert!(spec.json["components"]["securitySchemes"]["bearer"].is_object());

    let docs = common::send(&app, "GET", "/docs", None, None).await;
    assert_eq!(docs.status, StatusCode::OK);
    assert!(
        docs.headers[header::CONTENT_TYPE]
            .to_str()
            .unwrap()
            .starts_with("text/html")
    );
}

#[sqlx::test]
async fn docs_can_be_switched_off(db: PgPool) {
    let mut config = common::config();
    config.server.api_docs = false;
    let app = common::router(db, config);

    for path in ["/openapi.json", "/docs"] {
        let res = common::send(&app, "GET", path, None, None).await;
        assert_eq!(res.status, StatusCode::NOT_FOUND, "{path}");
    }
}
