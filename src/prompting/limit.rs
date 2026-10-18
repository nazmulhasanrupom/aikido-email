//! A small in-memory limiter for the routes that cost tokens: 10 calls per minute per user (or per API key).
use std::collections::{HashMap, VecDeque};
use std::sync::Mutex;
use std::time::{Duration, Instant};

pub const PER_MINUTE: usize = 10;

#[derive(Default)]
pub struct RateLimiter {
    seen: Mutex<HashMap<String, VecDeque<Instant>>>,
}

impl RateLimiter {
    /// `Ok` and the call is counted, or `Err(seconds to wait)`.
    pub fn check(&self, key: &str, max: usize, window: Duration) -> Result<(), u64> {
        let mut seen = self.seen.lock().unwrap_or_else(|e| e.into_inner());
        let now = Instant::now();
        let q = seen.entry(key.to_string()).or_default();
        while q.front().is_some_and(|t| now.duration_since(*t) >= window) {
            q.pop_front();
        }
        if q.len() >= max {
            let wait = window.saturating_sub(now.duration_since(*q.front().unwrap_or(&now)));
            return Err(wait.as_secs().max(1));
        }
        q.push_back(now);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_eleventh_call_in_a_minute_is_refused() {
        let l = RateLimiter::default();
        for _ in 0..PER_MINUTE {
            assert!(l.check("u1", PER_MINUTE, Duration::from_secs(60)).is_ok());
        }
        assert!(l.check("u1", PER_MINUTE, Duration::from_secs(60)).is_err());
        assert!(l.check("u2", PER_MINUTE, Duration::from_secs(60)).is_ok(), "another user is not affected");
    }
}
