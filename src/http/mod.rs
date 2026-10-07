//! HTTP routes, one module per resource. Public routes live under `/v1`,
//! admin routes under `/v1/admin`. Health probes sit at the root, outside rate
//! limiting, so load-balancer checks never get throttled.

use axum::{
    Router,
    http::{HeaderValue, header},
    middleware,
};
use tower_http::{services::ServeDir, set_header::SetResponseHeader};

use crate::{
    app::AppState,
    config::Quota,
    ratelimit::{self, Limit},
    storage,
};

mod admin_catalog;
mod admin_users;
mod auth;
mod catalog;
mod health;
mod setup;

pub fn routes(state: &AppState) -> Router<AppState> {
    let rl = &state.config.rate_limit;
    // Endpoints that take passwords or the setup token get a much tighter budget.
    let credentials = Router::new()
        .merge(auth::credential_routes())
        .merge(setup::credential_routes());
    let credentials = with_limit(credentials, state, "auth", rl.auth);

    let v1 = Router::new()
        .merge(auth::routes())
        .merge(admin_users::routes())
        .merge(admin_catalog::routes())
        .merge(catalog::routes())
        .merge(setup::routes())
        .merge(credentials);
    let v1 = with_limit(v1, state, "global", rl.global);

    let mut router = Router::new().nest("/v1", v1).merge(health::routes());
    if let Some(root) = state.storage.local_root() {
        router = router.nest_service("/media", media(root));
    }
    router
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
fn with_limit(router: Router<AppState>, state: &AppState, tier: &'static str, quota: Quota) -> Router<AppState> {
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
