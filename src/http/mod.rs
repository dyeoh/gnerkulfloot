//! HTTP routes, one module per resource. Public routes live under `/v1`,
//! admin routes under `/v1/admin`. Health probes sit at the root, outside rate
//! limiting, so load-balancer checks never get throttled.

use axum::{Router, middleware};

use crate::{
    app::AppState,
    config::Quota,
    ratelimit::{self, Limit},
};

mod admin_users;
mod auth;
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
        .merge(setup::routes())
        .merge(credentials);
    let v1 = with_limit(v1, state, "global", rl.global);

    Router::new().nest("/v1", v1).merge(health::routes())
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
