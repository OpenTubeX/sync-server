//! Fixed-window rate limiting for unauthenticated endpoints.
//!
//! `/account/register` and `/account/login` are reachable without credentials,
//! so without a limit they allow unlimited password guessing and unlimited
//! account creation (each account can then consume storage quota).
//!
//! The counters are per process and in memory. That is enough to blunt abuse
//! from a single source; an operator running multiple replicas or needing
//! stronger guarantees should also rate limit at the reverse proxy.

use std::collections::HashMap;
use std::net::IpAddr;
use std::sync::Mutex;
use std::time::{Duration, Instant};

/// Requests permitted per client bucket within one window.
pub const MAX_REQUESTS_PER_WINDOW: u32 = 10;
pub const WINDOW: Duration = Duration::from_secs(60);

/// Hard bound on tracked buckets, so a flood of distinct addresses cannot grow
/// the map without limit.
const MAX_TRACKED_CLIENTS: usize = 100_000;

/// Key a client by address, grouping IPv6 by its /64.
///
/// A single IPv6 allocation is routinely a /64 or larger, so keying on the full
/// address would let one host rotate through addresses for unlimited login
/// attempts. IPv4 is keyed on the full address.
fn client_bucket(client: IpAddr) -> [u8; 8] {
    match client {
        IpAddr::V4(address) => {
            let mut bucket = [0u8; 8];
            bucket[..4].copy_from_slice(&address.octets());
            // tag so that an IPv4 address cannot collide with a /64 prefix
            bucket[4] = 1;
            bucket
        }
        IpAddr::V6(address) => {
            let mut bucket = [0u8; 8];
            bucket.copy_from_slice(&address.octets()[..8]);
            bucket
        }
    }
}

struct Window {
    started_at: Instant,
    count: u32,
}

pub struct RateLimiter {
    windows: Mutex<HashMap<[u8; 8], Window>>,
    max_requests: u32,
    window: Duration,
    max_tracked_clients: usize,
    trust_forwarded_for: bool,
}

impl RateLimiter {
    pub fn new(max_requests: u32, window: Duration) -> Self {
        Self::with_capacity(max_requests, window, MAX_TRACKED_CLIENTS)
    }

    /// Same as [`RateLimiter::new`] with an explicit tracking cap, so that tests
    /// can exercise the overflow path without inserting 100_000 entries.
    pub fn with_capacity(max_requests: u32, window: Duration, max_tracked_clients: usize) -> Self {
        Self {
            windows: Mutex::new(HashMap::new()),
            max_requests,
            window,
            max_tracked_clients,
            trust_forwarded_for: false,
        }
    }

    /// Resolve client addresses from `X-Forwarded-For` instead of the peer.
    ///
    /// Carried here rather than read from the global config on every request, so
    /// that the middleware stays unit-testable.
    pub fn trusting_forwarded_for(mut self, trust: bool) -> Self {
        self.trust_forwarded_for = trust;
        self
    }

    pub fn trusts_forwarded_for(&self) -> bool {
        self.trust_forwarded_for
    }

    /// Record a request from `client` and report whether it is permitted.
    pub fn check(&self, client: IpAddr) -> bool {
        let now = Instant::now();
        let mut windows = self.windows.lock().expect("rate limiter mutex poisoned");

        // Bound the map. Expired entries go first; if a flood of distinct
        // addresses is still live beyond the cap, drop everything rather than
        // refusing unknown addresses, which would let an attacker with a large
        // address pool lock every legitimate user out of login.
        if windows.len() >= self.max_tracked_clients {
            windows.retain(|_, window| now.duration_since(window.started_at) < self.window);
            if windows.len() >= self.max_tracked_clients {
                log::warn!(
                    "rate limiter tracked more than {} live clients; resetting counters. \
                     Rate limit at the reverse proxy as well.",
                    self.max_tracked_clients
                );
                windows.clear();
            }
        }

        let entry = windows.entry(client_bucket(client)).or_insert(Window {
            started_at: now,
            count: 0,
        });

        if now.duration_since(entry.started_at) >= self.window {
            entry.started_at = now;
            entry.count = 0;
        }

        entry.count += 1;
        entry.count <= self.max_requests
    }
}

impl Default for RateLimiter {
    fn default() -> Self {
        Self::new(MAX_REQUESTS_PER_WINDOW, WINDOW)
    }
}

#[cfg(test)]
mod tests {
    use super::{RateLimiter, WINDOW};
    use std::net::{IpAddr, Ipv4Addr};
    use std::time::Duration;

    fn client(last_octet: u8) -> IpAddr {
        IpAddr::V4(Ipv4Addr::new(127, 0, 0, last_octet))
    }

    #[test]
    fn permits_up_to_the_limit_then_rejects() {
        let limiter = RateLimiter::new(3, WINDOW);

        assert!(limiter.check(client(1)));
        assert!(limiter.check(client(1)));
        assert!(limiter.check(client(1)));
        assert!(!limiter.check(client(1)));
        assert!(!limiter.check(client(1)));
    }

    #[test]
    fn tracks_clients_independently() {
        let limiter = RateLimiter::new(1, WINDOW);

        assert!(limiter.check(client(1)));
        assert!(!limiter.check(client(1)));
        // a different address still gets its own budget
        assert!(limiter.check(client(2)));
    }

    /// Rotating addresses inside one IPv6 /64 must not buy extra attempts.
    #[test]
    fn ipv6_addresses_share_a_64_bucket() {
        let limiter = RateLimiter::new(2, WINDOW);

        let first: IpAddr = "2001:db8::1".parse().unwrap();
        let second: IpAddr = "2001:db8::dead:beef".parse().unwrap();
        let other_prefix: IpAddr = "2001:db8:0:1::1".parse().unwrap();

        assert!(limiter.check(first));
        assert!(limiter.check(second));
        // budget for this /64 is now spent, whichever address is used
        assert!(!limiter.check(first));
        assert!(!limiter.check(second));
        // a different /64 is a different bucket
        assert!(limiter.check(other_prefix));
    }

    #[test]
    fn ipv4_does_not_collide_with_an_ipv6_prefix() {
        let limiter = RateLimiter::new(1, WINDOW);

        // an IPv6 /64 of all zeroes would otherwise share a key with 0.0.0.0
        let v4: IpAddr = "0.0.0.0".parse().unwrap();
        let v6: IpAddr = "::".parse().unwrap();

        assert!(limiter.check(v4));
        assert!(limiter.check(v6));
    }

    /// Overflow must stay bounded and fail open rather than locking out unknown
    /// clients. Uses a small cap so the eviction branch is genuinely reached.
    #[test]
    fn tracked_clients_stay_bounded_without_locking_clients_out() {
        const CAP: usize = 64;
        let limiter = RateLimiter::with_capacity(1, Duration::from_secs(3600), CAP);

        // every window is live, so eviction cannot reclaim anything and the
        // limiter must fall back to clearing
        let mut seen = 0usize;
        for octet_a in 0..=3u8 {
            for octet_b in 0..=255u8 {
                limiter.check(IpAddr::V4(Ipv4Addr::new(10, 0, octet_a, octet_b)));
                seen += 1;
            }
        }
        assert!(
            seen > CAP,
            "inserted {seen} clients, which must exceed the cap {CAP} to be meaningful"
        );

        let tracked = limiter.windows.lock().unwrap().len();
        assert!(tracked <= CAP, "tracked {tracked} buckets, cap {CAP}");

        // a fresh client is still served rather than rejected outright
        assert!(limiter.check(IpAddr::V4(Ipv4Addr::new(198, 51, 100, 1))));
    }

    #[test]
    fn budget_resets_after_the_window() {
        let limiter = RateLimiter::new(1, Duration::from_millis(20));

        assert!(limiter.check(client(1)));
        assert!(!limiter.check(client(1)));

        std::thread::sleep(Duration::from_millis(30));
        assert!(limiter.check(client(1)));
    }
}
