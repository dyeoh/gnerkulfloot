//! The handles the HTTP layer and the background worker share: the database
//! pool, config and the adapters chosen from it. Built once at startup in
//! `main.rs`; services take the pieces they need, not the whole state.

use std::sync::Arc;

use sqlx::PgPool;

use crate::{
    checkout::Pricing,
    config::Config,
    mail::{self, MailAdapter},
    payments::{self, PaymentAdapter},
    ratelimit::{MemoryRateLimiter, RateLimiter},
    shipping::{self, ShippingAdapter},
    storage::Storage,
};

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

    /// The config and adapters that pricing a basket needs.
    pub fn pricing(&self) -> Pricing<'_> {
        Pricing {
            shop: &self.config.shop,
            checkout: &self.config.checkout,
            tax: &self.config.tax,
            shipping: &self.shipping,
        }
    }
}
