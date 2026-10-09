//! The parts of the OpenAPI spec that aren't routes: API info, tags, the
//! security schemes, and the error responses every handler shares. Routes add
//! themselves to the spec where they're registered (see `http::routes`).

use utoipa::{
    Modify, OpenApi, ToResponse,
    openapi::{
        OpenApi as Spec,
        security::{ApiKey, ApiKeyValue, HttpAuthScheme, HttpBuilder, SecurityScheme},
    },
};

use crate::{error::Problem, money::Money};

/// The API description the routes are added to.
#[derive(OpenApi)]
#[openapi(
    info(
        title = "gnerkulfloot",
        description = "Headless shop API.\n\n\
            Money is always `{\"amount\", \"currency\"}`, with `amount` a whole number in the \
            currency's minor units (RM19.90 is `{\"amount\": 1990, \"currency\": \"MYR\"}`).\n\n\
            Errors are RFC 7807 problem details (`application/problem+json`). When a client needs \
            to know *what* failed, the problem has a stable `code`; branch on that, never on `detail`.\n\n\
            Every response carries an `x-request-id` header; quote it when reporting a problem.",
    ),
    modifiers(&SecuritySchemes),
    components(
        schemas(Problem, Money),
        responses(BadRequest, Unauthorized, Forbidden, NotFound, RateLimited),
    ),
    tags(
        (name = "catalog", description = "Browsing products and categories"),
        (name = "checkout", description = "Quoting baskets and placing orders"),
        (name = "orders", description = "Viewing orders and paying for them"),
        (name = "payments", description = "Payment provider callbacks"),
        (name = "auth", description = "Accounts and sessions"),
        (name = "setup", description = "First-time setup"),
        (name = "admin-catalog", description = "Managing products, SKUs, stock, images and categories"),
        (name = "admin-orders", description = "Managing orders"),
        (name = "admin-shipping", description = "Shipping zones, rates and tax rules"),
        (name = "admin-users", description = "Staff and admin accounts"),
        (name = "health", description = "Liveness and readiness probes"),
    ),
)]
pub struct ApiDoc;

/// The two ways a request proves who it is: a session token, or a guest's
/// order token.
struct SecuritySchemes;

impl Modify for SecuritySchemes {
    fn modify(&self, spec: &mut Spec) {
        let components = spec.components.get_or_insert_with(Default::default);
        components.add_security_scheme(
            "bearer",
            SecurityScheme::Http(
                HttpBuilder::new()
                    .scheme(HttpAuthScheme::Bearer)
                    .description(Some("Session token from `POST /v1/auth/login`, starting `gnk_`."))
                    .build(),
            ),
        );
        components.add_security_scheme(
            "order_token",
            SecurityScheme::ApiKey(ApiKey::Header(ApiKeyValue::with_description(
                "X-Order-Token",
                "A guest's key to their order: the `access_token` returned when it was placed.",
            ))),
        );
    }
}

// The error responses below exist only to describe `AppError` bodies in the
// spec; they're never constructed, hence `allow(dead_code)`.

/// The request was malformed or failed validation; `detail` says why.
#[allow(dead_code)]
#[derive(ToResponse)]
#[response(content_type = "application/problem+json")]
pub struct BadRequest(Problem);

/// No session token, or it has expired.
#[allow(dead_code)]
#[derive(ToResponse)]
#[response(content_type = "application/problem+json", headers(("WWW-Authenticate" = String)))]
pub struct Unauthorized(Problem);

/// Logged in, but this account's role isn't allowed to do this.
#[allow(dead_code)]
#[derive(ToResponse)]
#[response(content_type = "application/problem+json")]
pub struct Forbidden(Problem);

/// Nothing with that id (or slug) exists, or the caller may not see it.
#[allow(dead_code)]
#[derive(ToResponse)]
#[response(content_type = "application/problem+json")]
pub struct NotFound(Problem);

/// Too many requests from this client; wait `Retry-After` seconds.
#[allow(dead_code)]
#[derive(ToResponse)]
#[response(
    content_type = "application/problem+json",
    headers(("Retry-After" = u64, description = "Seconds to wait before retrying"))
)]
pub struct RateLimited(Problem);
