//! Small, allocation-free token bucket used per WebSocket connection.
//!
//! This limits accidental floods and makes chat spam boring. It is not a
//! replacement for network-level DDoS protection.

use std::time::{Duration, Instant};

#[derive(Debug)]
pub struct TokenBucket {
    tokens: f64,
    capacity: f64,
    refill_per_second: f64,
    last_refill: Instant,
}

impl TokenBucket {
    pub fn new(messages_per_second: u32, burst: u32) -> Self {
        let capacity = f64::from(burst.max(1));
        Self {
            tokens: capacity,
            capacity,
            refill_per_second: f64::from(messages_per_second.max(1)),
            last_refill: Instant::now(),
        }
    }

    pub fn allow(&mut self) -> bool {
        self.allow_at(Instant::now())
    }

    fn allow_at(&mut self, now: Instant) -> bool {
        let elapsed = now.saturating_duration_since(self.last_refill);
        self.last_refill = now;
        self.tokens =
            (self.tokens + elapsed.as_secs_f64() * self.refill_per_second).min(self.capacity);
        if self.tokens >= 1.0 {
            self.tokens -= 1.0;
            true
        } else {
            false
        }
    }
}

/// Applies a stricter budget to failed invite attempts per source IP.
#[derive(Debug)]
pub struct AuthFailures {
    failures: u32,
    window_started: Instant,
    blocked_until: Option<Instant>,
}

const AUTH_WINDOW: Duration = Duration::from_secs(60);
const AUTH_BLOCK: Duration = Duration::from_secs(60);
const AUTH_MAX_FAILURES: u32 = 5;

impl AuthFailures {
    pub fn new(now: Instant) -> Self {
        Self {
            failures: 0,
            window_started: now,
            blocked_until: None,
        }
    }

    pub fn is_blocked(&self, now: Instant) -> bool {
        self.blocked_until.is_some_and(|until| now < until)
    }

    /// Returns true when the caller should be blocked immediately.
    pub fn record_failure(&mut self, now: Instant) -> bool {
        if now.saturating_duration_since(self.window_started) > AUTH_WINDOW {
            self.failures = 0;
            self.window_started = now;
            self.blocked_until = None;
        }
        self.failures = self.failures.saturating_add(1);
        if self.failures >= AUTH_MAX_FAILURES {
            self.blocked_until = Some(now + AUTH_BLOCK);
            true
        } else {
            false
        }
    }

    pub fn expired(&self, now: Instant) -> bool {
        self.blocked_until.is_none_or(|until| now >= until)
            && now.saturating_duration_since(self.window_started) > AUTH_WINDOW
    }
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]
mod tests {
    use super::*;

    #[test]
    fn burst_is_limited_then_refilled() {
        let mut bucket = TokenBucket::new(10, 2);
        let start = Instant::now();
        bucket.last_refill = start;
        assert!(bucket.allow_at(start));
        assert!(bucket.allow_at(start));
        assert!(!bucket.allow_at(start));
        assert!(!bucket.allow_at(start + Duration::from_millis(50)));
        assert!(bucket.allow_at(start + Duration::from_millis(150)));
    }

    #[test]
    fn auth_failures_block_after_five_attempts() {
        let start = Instant::now();
        let mut failures = AuthFailures::new(start);
        for _ in 0..4 {
            assert!(!failures.record_failure(start));
        }
        assert!(failures.record_failure(start));
        assert!(failures.is_blocked(start + Duration::from_secs(1)));
        assert!(!failures.is_blocked(start + Duration::from_secs(61)));
    }

    #[test]
    fn auth_failure_window_resets() {
        let start = Instant::now();
        let mut failures = AuthFailures::new(start);
        for _ in 0..3 {
            failures.record_failure(start);
        }
        let later = start + Duration::from_secs(61);
        assert!(!failures.record_failure(later));
        assert_eq!(failures.failures, 1);
    }
}
