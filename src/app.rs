//! Shared application state and the router with its middleware stack.

use std::{sync::Arc, time::Duration};

use axum::{
    Router,
    extract::DefaultBodyLimit,
    http::{HeaderName, HeaderValue, Method, StatusCode, header},
};
use sqlx::PgPool;
use tower_http::{
    compression::CompressionLayer,
    cors::{AllowOrigin, CorsLayer},
    limit::RequestBodyLimitLayer,
    request_id::{MakeRequestUuid, PropagateRequestIdLayer, SetRequestIdLayer},
    timeout::TimeoutLayer,
    trace::TraceLayer,
};

use crate::{
    config::Config,
    http,
    mail::{self, MailAdapter},
    payments::{self, PaymentAdapter},
    ratelimit::{MemoryRateLimiter, RateLimiter},
    shipping::{self, ShippingAdapter},
    storage::Storage,
};

const REQUEST_ID: HeaderName = HeaderName::from_static("x-request-id");

/// Everything a handler may need. Cheap to clone: every field is a handle.
#[derive(Clone)]
pub struct AppState {
    pub db: PgPool,
    pub config: Arc<Config>,
    pub limiter: Arc<dyn RateLimiter>,
    pub storage: Storage,
    pub shipping: Arc<[Arc<dyn ShippingAdapter>]>,
    /// `None` when online payments aren't configured.
    pub payments: Option<Arc<dyn PaymentAdapter>>,
    /// Order and account emails.
    pub mail: Arc<dyn MailAdapter>,
}

impl AppState {
    /// Builds the state, choosing adapters from config.
    ///
    /// # Errors
    /// Fails if an adapter can't be set up, e.g. the media directory can't be created.
    pub fn new(db: PgPool, config: Config) -> anyhow::Result<Self> {
        // Redis-backed limiting arrives with the Redis adapter; until then each
        // instance limits on its own.
        let limiter: Arc<dyn RateLimiter> = MemoryRateLimiter::new();
        let storage = Storage::from_config(&config.storage)?;
        let shipping = shipping::from_config(&config.shipping.adapters, &db).into();
        let payments = payments::from_config(&config.payments);
        let mail = mail::from_config(&config.mail.transactional)?;
        Ok(Self {
            db,
            config: Arc::new(config),
            limiter,
            storage,
            shipping,
            payments,
            mail,
        })
    }
}

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
