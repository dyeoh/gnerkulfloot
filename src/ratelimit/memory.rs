//! In-process rate limiter. Correct for a single instance; with several
//! instances each keeps its own counters, so the effective limit multiplies by
//! the instance count until the Redis limiter is configured.

use std::{
    sync::{Arc, Weak},
    time::{Duration, Instant},
};

use async_trait::async_trait;
use dashmap::DashMap;

use super::{Decision, RateLimiter};
use crate::config::Quota;

/// GCRA ("generic cell rate algorithm") limiter. Per key it stores a single
/// timestamp, the theoretical arrival time (TAT) of the next request, instead of
/// a list of past requests, so memory stays flat no matter the traffic.
pub struct MemoryRateLimiter {
    tats: DashMap<String, Instant>,
}

impl MemoryRateLimiter {
    /// Creates the limiter and a background sweep that forgets idle clients.
    /// The sweep holds only a weak reference and stops when the limiter is dropped.
    pub fn new() -> Arc<Self> {
        let limiter = Arc::new(Self { tats: DashMap::new() });
        let weak: Weak<Self> = Arc::downgrade(&limiter);
        tokio::spawn(async move {
            let mut tick = tokio::time::interval(Duration::from_secs(60));
            loop {
                tick.tick().await;
                let Some(limiter) = weak.upgrade() else { break };
                let now = Instant::now();
                // A TAT in the past means the client's budget is full again,
                // which is the same as having no entry at all.
                limiter.tats.retain(|_, tat| *tat > now);
            }
        });
        limiter
    }

    fn check_at(&self, key: &str, quota: Quota, now: Instant) -> Decision {
        let interval = Duration::from_secs(60) / quota.per_minute.max(1);
        let window = interval * quota.burst.max(1);
        let mut tat = self.tats.entry(key.to_owned()).or_insert(now);
        let next = (*tat).max(now) + interval;
        if next > now + window {
            Decision::Denied {
                retry_after: next - window - now,
            }
        } else {
            *tat = next;
            Decision::Allowed
        }
    }
}

#[async_trait]
impl RateLimiter for MemoryRateLimiter {
    async fn check(&self, key: &str, quota: Quota) -> Decision {
        self.check_at(key, quota, Instant::now())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const Q: Quota = Quota {
        per_minute: 60,
        burst: 3,
    };

    #[tokio::test]
    async fn allows_burst_then_refills_at_rate() {
        let l = MemoryRateLimiter::new();
        let t0 = Instant::now();
        for _ in 0..3 {
            assert_eq!(l.check_at("k", Q, t0), Decision::Allowed);
        }
        let Decision::Denied { retry_after } = l.check_at("k", Q, t0) else {
            panic!("4th request should be denied")
        };
        assert_eq!(retry_after, Duration::from_secs(1)); // 60/min refills one per second
        assert_eq!(l.check_at("k", Q, t0 + Duration::from_secs(1)), Decision::Allowed);
        assert!(matches!(
            l.check_at("k", Q, t0 + Duration::from_secs(1)),
            Decision::Denied { .. }
        ));
    }

    #[tokio::test]
    async fn keys_are_independent() {
        let l = MemoryRateLimiter::new();
        let t0 = Instant::now();
        for _ in 0..3 {
            l.check_at("a", Q, t0);
        }
        assert!(matches!(l.check_at("a", Q, t0), Decision::Denied { .. }));
        assert_eq!(l.check_at("b", Q, t0), Decision::Allowed);
    }
}
