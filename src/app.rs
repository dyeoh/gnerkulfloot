//! The router with its middleware stack.

use std::time::Duration;

use axum::{
    Router,
    extract::DefaultBodyLimit,
    http::{HeaderName, HeaderValue, Method, StatusCode, header},
};
use tower_http::{
    compression::CompressionLayer,
    cors::{AllowOrigin, CorsLayer},
    limit::RequestBodyLimitLayer,
    request_id::{MakeRequestUuid, PropagateRequestIdLayer, SetRequestIdLayer},
    timeout::TimeoutLayer,
    trace::TraceLayer,
};

use crate::{http, state::AppState};

const REQUEST_ID: HeaderName = HeaderName::from_static("x-request-id");

/// Builds the full router: routes plus middleware, outermost layer last.
pub fn router(state: AppState) -> Router {
    let server = &state.config.server;

    // Each request gets an id (or keeps the one the proxy set), which is recorded
    // in its tracing span and echoed back so clients can quote it in bug reports.
    let trace = TraceLayer::new_for_http().make_span_with(|req: &axum::http::Request<_>| {
        let id = req
            .headers()
            .get(&REQUEST_ID)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("-");
        tracing::info_span!("request", method = %req.method(), path = %req.uri().path(), request_id = %id)
    });

    Router::new()
        .merge(http::routes(&state))
        .with_state(state.clone())
        .layer(CompressionLayer::new())
        .layer(cors(&state.config.server.cors_origins))
        // `server.body_limit_bytes` is the one body limit; axum's own 2 MB default
        // for extractors would otherwise reject ordinary phone photos.
        .layer(DefaultBodyLimit::disable())
        .layer(RequestBodyLimitLayer::new(server.body_limit_bytes))
        .layer(TimeoutLayer::with_status_code(
            StatusCode::REQUEST_TIMEOUT,
            Duration::from_secs(server.request_timeout_secs),
        ))
        .layer(PropagateRequestIdLayer::new(REQUEST_ID))
        .layer(trace)
        .layer(SetRequestIdLayer::new(REQUEST_ID, MakeRequestUuid))
}

/// Headless means the storefront lives on another origin, so browsers need CORS.
/// Only origins listed in config are allowed; an empty list disables CORS.
fn cors(origins: &[String]) -> CorsLayer {
    let origins: Vec<HeaderValue> = origins.iter().filter_map(|o| o.parse().ok()).collect();
    CorsLayer::new()
        .allow_origin(AllowOrigin::list(origins))
        .allow_methods([Method::GET, Method::POST, Method::PUT, Method::PATCH, Method::DELETE])
        .allow_headers([
            header::AUTHORIZATION,
            header::CONTENT_TYPE,
            HeaderName::from_static("idempotency-key"),
            // Guests' key to their order (GET /v1/orders/{id}, starting a payment).
            HeaderName::from_static("x-order-token"),
        ])
        .expose_headers([REQUEST_ID, header::RETRY_AFTER])
        .max_age(Duration::from_secs(3600))
}

#[cfg(test)]
mod tests {
    use axum::{body::Body, http::Request, routing::get};
    use tower::ServiceExt;

    use super::*;

    /// A browser storefront sends these headers; the preflight must allow every one,
    /// or the browser blocks the request before it reaches us.
    #[tokio::test]
    async fn cors_preflight_allows_the_storefront_headers() {
        let app = Router::new()
            .route("/", get(|| async {}))
            .layer(cors(&["https://shop.example".into()]));
        let preflight = Request::builder()
            .method(Method::OPTIONS)
            .uri("/")
            .header(header::ORIGIN, "https://shop.example")
            .header(header::ACCESS_CONTROL_REQUEST_METHOD, "POST")
            .header(
                header::ACCESS_CONTROL_REQUEST_HEADERS,
                "authorization,content-type,idempotency-key,x-order-token",
            )
            .body(Body::empty())
            .unwrap();
        let response = app.oneshot(preflight).await.unwrap();
        let allowed = response.headers()[header::ACCESS_CONTROL_ALLOW_HEADERS]
            .to_str()
            .unwrap();
        for name in ["authorization", "content-type", "idempotency-key", "x-order-token"] {
            assert!(allowed.contains(name), "{name} missing from {allowed}");
        }
    }
}
