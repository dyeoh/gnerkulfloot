//! The OpenAPI spec and the docs page built from it.

mod common;

use std::collections::BTreeSet;

use axum::http::{StatusCode, header};
use serde_json::Value;
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

/// Every route and method in the API. Adding or removing an endpoint should
/// change this list in the same commit, so the API surface never changes by
/// accident.
const OPERATIONS: &[&str] = &[
    "GET /healthz",
    "GET /readyz",
    "POST /v1/admin/categories",
    "DELETE /v1/admin/categories/{id}",
    "PATCH /v1/admin/categories/{id}",
    "DELETE /v1/admin/images/{id}",
    "PATCH /v1/admin/images/{id}",
    "GET /v1/admin/orders",
    "GET /v1/admin/orders/{id}",
    "POST /v1/admin/orders/{id}/cancel",
    "POST /v1/admin/orders/{id}/mark-paid",
    "GET /v1/admin/products",
    "POST /v1/admin/products",
    "GET /v1/admin/products/{id}",
    "PATCH /v1/admin/products/{id}",
    "POST /v1/admin/products/{id}/images",
    "POST /v1/admin/products/{id}/skus",
    "DELETE /v1/admin/shipping/rates/{id}",
    "PATCH /v1/admin/shipping/rates/{id}",
    "GET /v1/admin/shipping/zones",
    "POST /v1/admin/shipping/zones",
    "DELETE /v1/admin/shipping/zones/{id}",
    "PATCH /v1/admin/shipping/zones/{id}",
    "POST /v1/admin/shipping/zones/{id}/rates",
    "GET /v1/admin/skus/{id}",
    "PATCH /v1/admin/skus/{id}",
    "POST /v1/admin/skus/{id}/stock",
    "GET /v1/admin/tax-rules",
    "POST /v1/admin/tax-rules",
    "DELETE /v1/admin/tax-rules/{id}",
    "PATCH /v1/admin/tax-rules/{id}",
    "GET /v1/admin/users",
    "POST /v1/admin/users",
    "POST /v1/auth/login",
    "POST /v1/auth/logout",
    "GET /v1/auth/me",
    "POST /v1/auth/register",
    "GET /v1/categories",
    "POST /v1/checkout/quote",
    "GET /v1/me/orders",
    "POST /v1/orders",
    "GET /v1/orders/{id}",
    "POST /v1/orders/{id}/payment",
    "POST /v1/orders/{id}/payment/check",
    "POST /v1/payments/{adapter}/webhook",
    "GET /v1/products",
    "GET /v1/products/{slug}",
    "GET /v1/setup",
    "POST /v1/setup",
];

async fn spec(db: PgPool) -> Value {
    let app = common::router(db, common::config());
    common::send(&app, "GET", "/openapi.json", None, None).await.json
}

fn operations(spec: &Value) -> Vec<(String, &Value)> {
    let mut ops = Vec::new();
    for (path, item) in spec["paths"].as_object().unwrap() {
        for (method, op) in item.as_object().unwrap() {
            ops.push((format!("{} {path}", method.to_uppercase()), op));
        }
    }
    ops
}

#[sqlx::test]
async fn spec_lists_every_operation(db: PgPool) {
    let spec = spec(db).await;
    let documented: BTreeSet<String> = operations(&spec).into_iter().map(|(name, _)| name).collect();
    let expected: BTreeSet<String> = OPERATIONS.iter().map(|s| s.to_string()).collect();
    assert_eq!(
        documented.difference(&expected).collect::<Vec<_>>(),
        Vec::<&String>::new(),
        "documented but not in OPERATIONS"
    );
    assert_eq!(
        expected.difference(&documented).collect::<Vec<_>>(),
        Vec::<&String>::new(),
        "in OPERATIONS but not documented"
    );
}

/// What a client generator needs: unique operation ids, and every operation
/// summarised, tagged and with its responses described.
#[sqlx::test]
async fn every_operation_is_described(db: PgPool) {
    let spec = spec(db).await;
    let mut ids = BTreeSet::new();
    for (name, op) in operations(&spec) {
        let id = op["operationId"]
            .as_str()
            .unwrap_or_else(|| panic!("{name}: no operationId"));
        assert!(ids.insert(id.to_owned()), "{name}: operationId {id} is used twice");
        assert!(
            op["summary"].as_str().is_some_and(|s| !s.is_empty()),
            "{name}: no summary"
        );
        assert!(op["tags"].as_array().is_some_and(|t| !t.is_empty()), "{name}: no tag");
        assert!(
            op["responses"].as_object().is_some_and(|r| !r.is_empty()),
            "{name}: no responses"
        );
    }
}

/// A `$ref` to a schema or response that was never registered renders as an
/// empty type in the docs and breaks client generators.
#[sqlx::test]
async fn every_reference_resolves(db: PgPool) {
    fn refs<'a>(value: &'a Value, found: &mut Vec<&'a str>) {
        match value {
            Value::Object(map) => {
                for (key, v) in map {
                    match (key.as_str(), v.as_str()) {
                        ("$ref", Some(r)) => found.push(r),
                        _ => refs(v, found),
                    }
                }
            }
            Value::Array(items) => items.iter().for_each(|v| refs(v, found)),
            _ => {}
        }
    }
    let spec = spec(db).await;
    let mut found = Vec::new();
    refs(&spec, &mut found);
    assert!(!found.is_empty());
    for r in found {
        // "#/components/schemas/Money" → spec["components"]["schemas"]["Money"]
        let target = r
            .trim_start_matches("#/")
            .split('/')
            .fold(&spec, |node, key| &node[key]);
        assert!(!target.is_null(), "{r} doesn't resolve");
    }
}
