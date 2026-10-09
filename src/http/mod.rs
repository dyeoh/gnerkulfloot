//! HTTP routes, one module per resource. Public routes live under `/v1`,
//! admin routes under `/v1/admin`. Health probes sit at the root, outside rate
//! limiting, so load-balancer checks never get throttled.
//!
//! Every route is registered on an `OpenApiRouter`, which records it in the
//! OpenAPI spec as it adds it, so the spec served at `/openapi.json` (and
//! rendered at `/docs`) can't drift from the routes that actually exist.

use axum::{
    Json, Router,
    http::{HeaderValue, header},
    middleware,
    routing::get,
};
use tower_http::{services::ServeDir, set_header::SetResponseHeader};
use utoipa::OpenApi;
use utoipa_axum::router::OpenApiRouter;
use utoipa_scalar::{Scalar, Servable};

use crate::{
    config::Quota,
    ratelimit::{self, Limit},
    state::AppState,
    storage,
};

mod admin_catalog;
mod admin_orders;
mod admin_shipping;
mod admin_users;
mod auth;
mod catalog;
mod checkout;
pub mod docs;
mod health;
mod payments;
mod setup;

pub fn routes(state: &AppState) -> Router<AppState> {
    let (mut router, spec) = api_routes(state).split_for_parts();
    if let Some(root) = state.storage.local_root() {
        router = router.nest_service("/media", media(root));
    }
    if !state.config.server.api_docs {
        return router;
    }
    // Served outside every rate limit, like the health probes.
    let json = Json(spec.clone());
    router
        .route("/openapi.json", get(move || async move { json }))
        .merge(Scalar::with_url("/docs", spec))
}

fn api_routes(state: &AppState) -> OpenApiRouter<AppState> {
    let rl = &state.config.rate_limit;
    // Endpoints that take passwords or the setup token get a much tighter budget.
    let credentials = OpenApiRouter::new()
        .merge(auth::credential_routes())
        .merge(setup::credential_routes());
    let credentials = with_limit(credentials, state, "auth", rl.auth);
    // Placing an order holds stock, so it gets its own budget.
    let ordering = with_limit(checkout::order_routes(), state, "orders", rl.orders);

    let v1 = OpenApiRouter::new()
        .merge(auth::routes())
        .merge(admin_users::routes())
        .merge(admin_catalog::routes())
        .merge(admin_orders::routes())
        .merge(admin_shipping::routes())
        .merge(catalog::routes())
        .merge(checkout::routes())
        .merge(ordering)
        .merge(setup::routes())
        .merge(credentials);
    let v1 = with_limit(v1, state, "global", rl.global);

    // Provider webhooks sit outside every rate limit; see http/payments.rs.
    let v1 = v1.merge(payments::routes());
    OpenApiRouter::with_openapi(docs::ApiDoc::openapi())
        .nest("/v1", v1)
        .merge(health::routes())
}

/// Serves locally stored images. Files are content-addressed and never
/// change, so browsers may cache them for a year.
fn media(root: &std::path::Path) -> SetResponseHeader<ServeDir, HeaderValue> {
    SetResponseHeader::overriding(
        ServeDir::new(root),
        header::CACHE_CONTROL,
        HeaderValue::from_static(storage::IMMUTABLE_CACHE),
    )
}

/// Applies a rate-limit tier to every route in `router` (no-op when rate limiting is disabled).
fn with_limit(
    router: OpenApiRouter<AppState>,
    state: &AppState,
    tier: &'static str,
    quota: Quota,
) -> OpenApiRouter<AppState> {
    if !state.config.rate_limit.enabled {
        return router;
    }
    let limit = Limit {
        limiter: state.limiter.clone(),
        tier,
        quota,
        trusted_proxies: state.config.server.trusted_proxies.clone().into(),
    };
    router.route_layer(middleware::from_fn_with_state(limit, ratelimit::enforce))
}
