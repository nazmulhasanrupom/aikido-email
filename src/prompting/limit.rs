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

/// 10 calls a minute per user (or API key) for the routes that cost tokens.
pub fn check_user(st: &crate::state::AppState, who: Option<uuid::Uuid>) -> crate::error::ApiResult<()> {
    let key = who.map(|u| u.to_string()).unwrap_or_else(|| "key".to_string());
    st.ai_limiter
        .check(&key, PER_MINUTE, Duration::from_secs(60))
        .map_err(|wait| crate::error::ApiError::too_many("rate_limited", "This costs tokens. You can do it 10 times a minute. Wait a moment.", wait as i64))
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
