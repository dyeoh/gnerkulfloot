//! Typed configuration, loaded from an optional TOML file and overridden by
//! `GNK__SECTION__KEY` environment variables. Adapter sections select their
//! implementation with an `adapter` key; those sections are added as each
//! adapter lands.

use std::{
    net::SocketAddr,
    path::{Path, PathBuf},
};

use figment::{
    Figment,
    providers::{Env, Format, Serialized, Toml},
};
use ipnet::IpNet;
use iso_currency::Currency;
use serde::{Deserialize, Serialize};

/// Root configuration for one gnerkulfloot instance.
#[derive(Clone, Deserialize, Serialize)]
pub struct Config {
    #[serde(default)]
    pub server: ServerConfig,
    pub database: DatabaseConfig,
    #[serde(default)]
    pub shop: ShopConfig,
    #[serde(default)]
    pub auth: AuthConfig,
    #[serde(default)]
    pub setup: SetupConfig,
    #[serde(default)]
    pub rate_limit: RateLimitConfig,
    #[serde(default)]
    pub storage: StorageConfig,
}

/// HTTP server behaviour.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(default)]
pub struct ServerConfig {
    pub bind: SocketAddr,
    pub log_format: LogFormat,
    /// Requests running longer than this are cut off with 408.
    pub request_timeout_secs: u64,
    /// Upper bound on any request body. Image uploads are the largest legitimate bodies.
    pub body_limit_bytes: usize,
    /// Browser origins allowed to call the API (your storefront). Empty means no CORS.
    pub cors_origins: Vec<String>,
    /// Run pending migrations on startup. Safe with many instances: sqlx takes an advisory lock.
    pub migrate_on_start: bool,
    /// Proxies/load balancers whose `X-Forwarded-For` header we believe, as CIDRs
    /// (e.g. `10.0.0.0/8`). Anyone else could forge the header to dodge rate limits.
    pub trusted_proxies: Vec<IpNet>,
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            bind: SocketAddr::from(([0, 0, 0, 0], 8080)),
            log_format: LogFormat::Text,
            request_timeout_secs: 30,
            body_limit_bytes: 10 * 1024 * 1024,
            cors_origins: Vec::new(),
            migrate_on_start: true,
            trusted_proxies: Vec::new(),
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum LogFormat {
    /// Human-readable, for local development.
    Text,
    /// One JSON object per line, for log shippers in production.
    Json,
}

/// Postgres connection settings. Deliberately not `Debug`: the URL holds a password.
#[derive(Clone, Deserialize, Serialize)]
pub struct DatabaseConfig {
    pub url: String,
    #[serde(default = "default_max_connections")]
    pub max_connections: u32,
}

fn default_max_connections() -> u32 {
    20
}

/// Shop-wide defaults.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(default)]
pub struct ShopConfig {
    pub name: String,
    /// Currency used when a request doesn't ask for one. Any ISO 4217 code.
    pub default_currency: Currency,
}

impl Default for ShopConfig {
    fn default() -> Self {
        Self {
            name: "gnerkulfloot".into(),
            default_currency: Currency::USD,
        }
    }
}

/// Login and session behaviour.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(default)]
pub struct AuthConfig {
    /// How long a login stays valid.
    pub session_ttl_hours: i64,
    pub min_password_length: usize,
    /// Consecutive wrong passwords before an account is locked for a while.
    pub max_failed_logins: i32,
    pub lockout_minutes: i64,
}

impl Default for AuthConfig {
    fn default() -> Self {
        Self {
            session_ttl_hours: 24 * 30,
            min_password_length: 10,
            max_failed_logins: 5,
            lockout_minutes: 15,
        }
    }
}

/// First-time setup. Not `Debug`: it may hold the setup token.
#[derive(Clone, Default, Deserialize, Serialize)]
#[serde(default)]
pub struct SetupConfig {
    /// Use this setup token instead of a generated one. Handy for automated
    /// deployments; otherwise leave unset and read the token from the logs.
    pub token: Option<String>,
}

/// Request rate limits per client IP. Each tier allows `burst` requests at once,
/// refilling at `per_minute`.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(default)]
pub struct RateLimitConfig {
    pub enabled: bool,
    /// Every request.
    pub global: Quota,
    /// Login, registration and first-time setup: the endpoints worth brute-forcing.
    pub auth: Quota,
}

impl Default for RateLimitConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            global: Quota {
                per_minute: 300,
                burst: 100,
            },
            auth: Quota {
                per_minute: 10,
                burst: 5,
            },
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct Quota {
    pub per_minute: u32,
    pub burst: u32,
}

/// Where uploaded images are stored, picked with `adapter`. Not `Debug`: holds keys.
#[derive(Clone, Deserialize, Serialize)]
#[serde(tag = "adapter", rename_all = "snake_case")]
pub enum StorageConfig {
    /// A directory on this server, served by the app at `/media`. Fine for one
    /// instance; with several, use `s3` so every instance sees every image.
    Local {
        #[serde(default = "default_media_path")]
        path: PathBuf,
        /// Where browsers fetch files from. Make it absolute (`https://api.example.com/media`)
        /// when the storefront runs on another domain.
        #[serde(default = "default_media_url")]
        public_base_url: String,
    },
    /// Any S3-compatible bucket: AWS S3, Cloudflare R2, DigitalOcean Spaces, MinIO.
    S3 {
        bucket: String,
        /// `auto` for R2; the datacenter (e.g. `sgp1`) for DO Spaces.
        region: String,
        /// Leave unset for AWS. R2: `https://<account>.r2.cloudflarestorage.com`.
        /// Spaces: `https://<region>.digitaloceanspaces.com`.
        endpoint: Option<String>,
        access_key_id: String,
        secret_access_key: String,
        /// Public URL of the bucket or its CDN; image URLs are built from it.
        public_base_url: String,
        /// Needed for MinIO over plain http in development.
        #[serde(default)]
        allow_http: bool,
    },
}

fn default_media_path() -> PathBuf {
    PathBuf::from("media")
}

fn default_media_url() -> String {
    "/media".into()
}

impl Default for StorageConfig {
    fn default() -> Self {
        StorageConfig::Local {
            path: default_media_path(),
            public_base_url: default_media_url(),
        }
    }
}

impl Config {
    /// Loads defaults, then the TOML file at `path` if it exists, then `GNK__*` env vars.
    ///
    /// # Errors
    /// Fails when a value has the wrong type or a required key (such as
    /// `database.url`) is missing from every source.
    pub fn load(path: &Path) -> Result<Self, Box<figment::Error>> {
        Figment::from(Serialized::default("server", ServerConfig::default()))
            // Lets a deployment set a single storage key (e.g. GNK__STORAGE__PATH)
            // without also having to say `adapter = "local"`.
            .merge(Serialized::default("storage.adapter", "local"))
            .merge(Toml::file(path))
            .merge(Env::prefixed("GNK__").split("__"))
            .extract()
            .map_err(Box::new)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[allow(clippy::result_large_err)] // figment::Jail's closure signature is fixed by the crate.
    fn env_overrides_nested_keys() {
        figment::Jail::expect_with(|jail| {
            jail.set_env("GNK__DATABASE__URL", "postgres://x");
            jail.set_env("GNK__SERVER__BIND", "127.0.0.1:9000");
            jail.set_env("GNK__SHOP__DEFAULT_CURRENCY", "MYR");
            let cfg = Config::load(Path::new("missing.toml")).unwrap();
            assert_eq!(cfg.database.url, "postgres://x");
            assert_eq!(cfg.server.bind.port(), 9000);
            assert_eq!(cfg.shop.default_currency, Currency::MYR);
            Ok(())
        });
    }

    #[test]
    #[allow(clippy::result_large_err)]
    fn storage_adapter_defaults_to_local_when_only_some_keys_are_set() {
        figment::Jail::expect_with(|jail| {
            jail.set_env("GNK__DATABASE__URL", "postgres://x");
            jail.set_env("GNK__STORAGE__PATH", "/data/media");
            let cfg = Config::load(Path::new("missing.toml")).unwrap();
            assert!(matches!(cfg.storage, StorageConfig::Local { ref path, .. } if path == Path::new("/data/media")));
            Ok(())
        });
    }

    #[test]
    #[allow(clippy::result_large_err)]
    fn storage_can_switch_to_s3_from_env() {
        figment::Jail::expect_with(|jail| {
            jail.set_env("GNK__DATABASE__URL", "postgres://x");
            jail.set_env("GNK__STORAGE__PATH", "/data/media"); // left over from the image; ignored
            for (k, v) in [
                ("ADAPTER", "s3"),
                ("BUCKET", "media"),
                ("REGION", "auto"),
                ("ACCESS_KEY_ID", "k"),
                ("SECRET_ACCESS_KEY", "s"),
                ("PUBLIC_BASE_URL", "https://cdn.example.com"),
            ] {
                jail.set_env(format!("GNK__STORAGE__{k}"), v);
            }
            let cfg = Config::load(Path::new("missing.toml")).unwrap();
            assert!(matches!(cfg.storage, StorageConfig::S3 { ref bucket, .. } if bucket == "media"));
            Ok(())
        });
    }
}
