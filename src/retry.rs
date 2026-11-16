//! Exponential backoff with full jitter (architecture §7.2).

use std::time::Duration;

use crate::config::RetryConfig;

pub const MIN_DELAY: Duration = Duration::from_millis(100);

#[derive(Debug, Clone, Copy)]
pub struct Backoff {
    base: Duration,
    max: Duration,
}

impl Backoff {
    pub fn new(base: Duration, max: Duration) -> Self {
        Self {
            base,
            max: max.max(MIN_DELAY),
        }
    }

    pub fn from_config(cfg: &RetryConfig) -> Self {
        Self::new(cfg.base_delay, cfg.max_delay)
    }

    /// `min(max, base × 2^attempt)` without jitter, never below [`MIN_DELAY`].
    pub fn ceiling(&self, attempt: u32) -> Duration {
        let factor = 1u32.checked_shl(attempt.min(31)).unwrap_or(u32::MAX);
        self.base
            .checked_mul(factor)
            .unwrap_or(self.max)
            .min(self.max)
            .max(MIN_DELAY)
    }

    /// Full jitter: uniform in `0..=ceiling`, clamped to at least [`MIN_DELAY`].
    pub fn delay(&self, attempt: u32) -> Duration {
        let ceiling = self.ceiling(attempt).as_millis() as u64;
        let ms = rand::random_range(0..=ceiling);
        Duration::from_millis(ms).max(MIN_DELAY)
    }

    pub fn max(&self) -> Duration {
        self.max
    }
}

/// Extra wait on top of a server-provided `Retry-After` / `retry_after`.
pub fn rate_limit_jitter() -> Duration {
    Duration::from_millis(rand::random_range(0..=1000))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ceiling_grows_exponentially_and_caps() {
        let b = Backoff::new(Duration::from_secs(1), Duration::from_secs(60));
        assert_eq!(b.ceiling(0), Duration::from_secs(1));
        assert_eq!(b.ceiling(1), Duration::from_secs(2));
        assert_eq!(b.ceiling(5), Duration::from_secs(32));
        assert_eq!(b.ceiling(6), Duration::from_secs(60));
        assert_eq!(b.ceiling(1000), Duration::from_secs(60));
    }

    #[test]
    fn delay_is_within_bounds() {
        let b = Backoff::new(Duration::from_millis(10), Duration::from_secs(5));
        for attempt in 0..40 {
            for _ in 0..50 {
                let d = b.delay(attempt);
                assert!(d >= MIN_DELAY, "{d:?}");
                assert!(d <= b.ceiling(attempt).max(MIN_DELAY), "{d:?}");
            }
        }
    }

    #[test]
    fn tiny_base_still_waits_minimum() {
        let b = Backoff::new(Duration::from_millis(1), Duration::from_millis(1));
        assert_eq!(b.ceiling(0), MIN_DELAY);
        assert!(b.delay(0) >= MIN_DELAY);
    }

    #[test]
    fn jitter_is_at_most_one_second() {
        for _ in 0..100 {
            assert!(rate_limit_jitter() <= Duration::from_secs(1));
        }
    }
}
