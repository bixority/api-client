use crate::types::Method;
use std::collections::hash_map::RandomState;
use std::hash::{BuildHasher, Hasher};
use std::time::Duration;

/// When and how a request is retried.
///
/// The default reproduces the behaviour of 0.1: up to 3 attempts, `GET`
/// only, on connect errors, timeouts and request errors, and on the statuses
/// 502, 503 and 504, with 100 ms and then 200 ms between the attempts.
#[derive(Clone, Debug, PartialEq, Eq)]
#[allow(clippy::struct_excessive_bools)]
pub struct RetryPolicy {
    /// The most attempts, the first included. `0` and `1` mean no retry.
    pub max_attempts: u32,
    /// The wait after the first failed attempt; it doubles with each further one.
    pub base_delay: Duration,
    /// The longest wait between two attempts, also the cap of a `Retry-After` wait.
    pub max_delay: Duration,
    /// Wait a random time between half the delay and the whole delay.
    pub jitter: bool,
    /// Retry a request that timed out.
    pub retry_timeouts: bool,
    /// Wait as long as the `Retry-After` header (delta-seconds form) asks,
    /// at most `max_delay`.
    pub respect_retry_after: bool,
    /// Also retry `HEAD`, `PUT` and `DELETE`, not only `GET`.
    pub idempotent_methods: bool,
    /// The response statuses that are retried.
    pub retry_statuses: Vec<u16>,
}

impl Default for RetryPolicy {
    fn default() -> Self {
        Self {
            max_attempts: 3,
            base_delay: Duration::from_millis(100),
            max_delay: Duration::from_secs(2),
            jitter: false,
            retry_timeouts: true,
            respect_retry_after: false,
            idempotent_methods: false,
            retry_statuses: vec![502, 503, 504],
        }
    }
}

impl RetryPolicy {
    /// A policy that never retries.
    #[must_use]
    pub fn none() -> Self {
        Self {
            max_attempts: 1,
            ..Self::default()
        }
    }

    /// Whether a request with `method` may be retried at all.
    pub(crate) const fn method_retryable(&self, method: Method) -> bool {
        match method {
            Method::Get => true,
            Method::Head | Method::Put | Method::Delete => self.idempotent_methods,
            Method::Post | Method::Patch => false,
        }
    }

    pub(crate) fn status_retryable(&self, status: u16) -> bool {
        self.retry_statuses.contains(&status)
    }

    /// The wait after failed attempt `attempt` (1-based): `retry_after` when
    /// honoured, else `base_delay * 2^(attempt - 1)`, both at most `max_delay`.
    pub(crate) fn delay(&self, attempt: u32, retry_after: Option<Duration>) -> Duration {
        if self.respect_retry_after
            && let Some(wait) = retry_after
        {
            return wait.min(self.max_delay);
        }
        let shift = attempt.saturating_sub(1).min(31);
        let delay = self
            .base_delay
            .saturating_mul(1u32 << shift)
            .min(self.max_delay);
        if self.jitter {
            jittered(delay, random_u64())
        } else {
            delay
        }
    }
}

/// A value in `[delay / 2, delay]` picked by `r`.
pub fn jittered(delay: Duration, r: u64) -> Duration {
    let half = delay / 2;
    let span = u64::try_from(delay.saturating_sub(half).as_nanos()).unwrap_or(u64::MAX);
    half + Duration::from_nanos(r % span.saturating_add(1))
}

fn random_u64() -> u64 {
    RandomState::new().build_hasher().finish()
}

/// The `Retry-After` wait of a header value in delta-seconds form; the HTTP
/// date form is ignored.
pub fn parse_retry_after(value: &str) -> Option<Duration> {
    value.trim().parse::<u64>().ok().map(Duration::from_secs)
}
