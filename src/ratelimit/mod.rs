//! Per-client request rate limiting. The `RateLimiter` trait hides where the
//! counters live: in this process (`MemoryRateLimiter`) or, once added, in Redis
//! so that several instances share one budget. Both use the same GCRA maths, so
//! switching changes scope, not behaviour.

mod memory;

use std::{
    net::{IpAddr, SocketAddr},
    sync::Arc,
    time::Duration,
};

use async_trait::async_trait;
use axum::{
    extract::{ConnectInfo, Request, State},
    http::HeaderMap,
    middleware::Next,
    response::Response,
};
use ipnet::IpNet;

pub use memory::MemoryRateLimiter;

use crate::{config::Quota, error::AppError};

/// Whether a request may go ahead.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Decision {
    Allowed,
    /// Denied; the caller may try again after this long.
    Denied {
        retry_after: Duration,
    },
}

#[async_trait]
pub trait RateLimiter: Send + Sync {
    /// Counts one request against `key` and says whether it fits in `quota`.
    ///
    /// Infallible by design: an implementation whose backing store is down
    /// should log and allow, because an outage of the limiter must not take the
    /// shop down with it.
    async fn check(&self, key: &str, quota: Quota) -> Decision;
}

/// Middleware state for one rate-limit tier (e.g. "global" or "auth").
#[derive(Clone)]
pub struct Limit {
    pub limiter: Arc<dyn RateLimiter>,
    pub tier: &'static str,
    pub quota: Quota,
    pub trusted_proxies: Arc<[IpNet]>,
}

/// Axum middleware: rejects with 429 and `Retry-After` once a client exceeds the tier's quota.
pub async fn enforce(State(limit): State<Limit>, req: Request, next: Next) -> Result<Response, AppError> {
    let peer = req.extensions().get::<ConnectInfo<SocketAddr>>().map(|c| c.0.ip());
    let client = client_ip(peer, req.headers(), &limit.trusted_proxies);
    let key = match client {
        Some(ip) => format!("{}:{}", limit.tier, bucket(ip)),
        None => format!("{}:unknown", limit.tier),
    };
    match limit.limiter.check(&key, limit.quota).await {
        Decision::Allowed => Ok(next.run(req).await),
        Decision::Denied { retry_after } => Err(AppError::RateLimited {
            retry_after_secs: retry_after.as_secs_f64().ceil() as u64,
        }),
    }
}

/// Works out who the client really is.
///
/// `X-Forwarded-For` is only believed when the direct peer is a trusted proxy;
/// otherwise anyone could put a random IP in it and get a fresh rate-limit
/// budget per request. The header is read right to left, skipping our own
/// proxies, and the first address that isn't one of them is the client.
pub fn client_ip(peer: Option<IpAddr>, headers: &HeaderMap, trusted: &[IpNet]) -> Option<IpAddr> {
    let peer = peer?;
    let is_trusted = |ip: &IpAddr| trusted.iter().any(|net| net.contains(ip));
    if !is_trusted(&peer) {
        return Some(peer);
    }
    let forwarded: Vec<IpAddr> = headers
        .get_all("x-forwarded-for")
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(','))
        .filter_map(|s| s.trim().parse().ok())
        .collect();
    forwarded
        .iter()
        .rev()
        .find(|ip| !is_trusted(ip))
        .or(forwarded.first())
        .copied()
        .or(Some(peer))
}

/// Rate-limit bucket for an address. IPv6 users typically control a whole /64,
/// so limiting single addresses would let them rotate freely.
fn bucket(ip: IpAddr) -> String {
    match ip {
        IpAddr::V4(v4) => v4.to_string(),
        IpAddr::V6(v6) => {
            let s = v6.segments();
            format!("{:x}:{:x}:{:x}:{:x}::/64", s[0], s[1], s[2], s[3])
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn headers(xff: &str) -> HeaderMap {
        let mut h = HeaderMap::new();
        h.insert("x-forwarded-for", xff.parse().unwrap());
        h
    }

    fn ip(s: &str) -> IpAddr {
        s.parse().unwrap()
    }

    #[test]
    fn ignores_forwarded_header_from_untrusted_peer() {
        let trusted: Vec<IpNet> = vec!["10.0.0.0/8".parse().unwrap()];
        let got = client_ip(Some(ip("203.0.113.9")), &headers("1.1.1.1"), &trusted);
        assert_eq!(got, Some(ip("203.0.113.9")));
    }

    #[test]
    fn takes_rightmost_untrusted_hop_behind_trusted_proxy() {
        let trusted: Vec<IpNet> = vec!["10.0.0.0/8".parse().unwrap()];
        // Client forged "6.6.6.6"; the real client is what our proxy appended.
        let got = client_ip(
            Some(ip("10.0.0.2")),
            &headers("6.6.6.6, 198.51.100.7, 10.0.0.3"),
            &trusted,
        );
        assert_eq!(got, Some(ip("198.51.100.7")));
    }

    #[test]
    fn falls_back_to_peer_without_header() {
        let trusted: Vec<IpNet> = vec!["10.0.0.0/8".parse().unwrap()];
        let got = client_ip(Some(ip("10.0.0.2")), &HeaderMap::new(), &trusted);
        assert_eq!(got, Some(ip("10.0.0.2")));
    }

    #[test]
    fn groups_ipv6_by_slash_64() {
        assert_eq!(bucket(ip("2001:db8:1:2:aaaa::1")), bucket(ip("2001:db8:1:2:bbbb::9")));
        assert_ne!(bucket(ip("2001:db8:1:2::1")), bucket(ip("2001:db8:1:3::1")));
    }
}
