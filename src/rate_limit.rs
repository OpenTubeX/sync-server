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

/// Requests permitted per client address within one window.
pub const MAX_REQUESTS_PER_WINDOW: u32 = 10;
pub const WINDOW: Duration = Duration::from_secs(60);

/// Entries are dropped once a window elapses, but a burst of distinct addresses
/// would still grow the map, so cap how many we track at once.
const MAX_TRACKED_CLIENTS: usize = 100_000;

struct Window {
    started_at: Instant,
    count: u32,
}

pub struct RateLimiter {
    windows: Mutex<HashMap<IpAddr, Window>>,
    max_requests: u32,
    window: Duration,
}

impl RateLimiter {
    pub fn new(max_requests: u32, window: Duration) -> Self {
        Self {
            windows: Mutex::new(HashMap::new()),
            max_requests,
            window,
        }
    }

    /// Record a request from `client` and report whether it is permitted.
    pub fn check(&self, client: IpAddr) -> bool {
        let now = Instant::now();
        let mut windows = self.windows.lock().expect("rate limiter mutex poisoned");

        // Drop expired entries so the map does not grow without bound.
        if windows.len() >= MAX_TRACKED_CLIENTS {
            windows.retain(|_, window| now.duration_since(window.started_at) < self.window);
        }

        let entry = windows.entry(client).or_insert(Window {
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

    #[test]
    fn budget_resets_after_the_window() {
        let limiter = RateLimiter::new(1, Duration::from_millis(20));

        assert!(limiter.check(client(1)));
        assert!(!limiter.check(client(1)));

        std::thread::sleep(Duration::from_millis(30));
        assert!(limiter.check(client(1)));
    }
}
