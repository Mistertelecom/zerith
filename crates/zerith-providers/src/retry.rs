//! Bounded retry for the request phase only.
//!
//! A retry happens before the first byte of an answer is read: once a stream
//! started painting text, repeating it would show the answer twice. So this
//! wraps `send`, never the stream loop.

use crate::{Cancel, ProviderError, Result};
use std::time::Duration;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RetryPolicy {
    /// Extra attempts after the first. 0 disables retry.
    pub max_retries: u32,
    pub base_ms: u64,
    pub max_ms: u64,
    /// A `Retry-After` longer than this is not waited for: the error goes to
    /// the user, who can decide to wait a minute better than a spinner can.
    pub max_retry_after_ms: u64,
}

impl Default for RetryPolicy {
    fn default() -> Self {
        Self {
            max_retries: 3,
            base_ms: 500,
            max_ms: 8_000,
            max_retry_after_ms: 30_000,
        }
    }
}

impl RetryPolicy {
    pub fn none() -> Self {
        Self {
            max_retries: 0,
            ..Self::default()
        }
    }

    /// Exponential backoff with full jitter in [delay/2, delay].
    fn backoff(&self, attempt: u32) -> Duration {
        let exp = self
            .base_ms
            .saturating_mul(1u64 << attempt.min(16))
            .min(self.max_ms);
        let half = exp / 2;
        Duration::from_millis(half + jitter(half + 1))
    }
}

/// Stdlib-only randomness: `RandomState` is seeded per instance by the OS.
/// Backoff spread only, never anything security-relevant.
fn jitter(bound: u64) -> u64 {
    use std::hash::{BuildHasher, Hasher};
    let mut h = std::collections::hash_map::RandomState::new().build_hasher();
    h.write_u64(bound);
    h.finish() % bound.max(1)
}

/// Whether a failed send may be repeated, and how long the server asked us to
/// wait.
fn retryable(e: &ureq::Error) -> Option<Option<u64>> {
    match e {
        ureq::Error::Status(status, r) if *status == 429 || *status >= 500 => {
            Some(r.header("retry-after").and_then(parse_retry_after))
        }
        ureq::Error::Transport(t) => match t.kind() {
            ureq::ErrorKind::ConnectionFailed | ureq::ErrorKind::Io => Some(None),
            _ => None,
        },
        _ => None,
    }
}

/// Seconds only; the HTTP-date form is rare from APIs and treated as absent.
fn parse_retry_after(v: &str) -> Option<u64> {
    v.trim()
        .parse::<f64>()
        .ok()
        .filter(|s| *s >= 0.0)
        .map(|s| (s * 1000.0) as u64)
}

/// Sleeps in short slices so a cancel during backoff is honoured quickly.
fn sleep(d: Duration, cancel: &Cancel) -> Result<()> {
    let deadline = std::time::Instant::now() + d;
    loop {
        if cancel.is_cancelled() {
            return Err(ProviderError::Cancelled);
        }
        let now = std::time::Instant::now();
        if now >= deadline {
            return Ok(());
        }
        std::thread::sleep((deadline - now).min(Duration::from_millis(25)));
    }
}

pub(crate) fn send(
    policy: &RetryPolicy,
    cancel: &Cancel,
    mut attempt_fn: impl FnMut() -> std::result::Result<ureq::Response, ureq::Error>,
) -> Result<ureq::Response> {
    let mut attempt = 0;
    loop {
        if cancel.is_cancelled() {
            return Err(ProviderError::Cancelled);
        }
        let err = match attempt_fn() {
            Ok(r) => return Ok(r),
            Err(e) => e,
        };
        let wait = match retryable(&err) {
            Some(after) if attempt < policy.max_retries => match after {
                Some(ms) if ms > policy.max_retry_after_ms => None,
                Some(ms) => Some(Duration::from_millis(ms)),
                None => Some(policy.backoff(attempt)),
            },
            _ => None,
        };
        match wait {
            Some(d) => {
                sleep(d, cancel)?;
                attempt += 1;
            }
            None => return Err(map_err(err)),
        }
    }
}

/// A 4xx/5xx is a provider answer, not a transport bug: keep the body so the
/// caller can show what the endpoint actually said.
pub(crate) fn map_err(e: ureq::Error) -> ProviderError {
    match e {
        ureq::Error::Status(429, r) => ProviderError::RateLimited {
            retry_after_ms: r.header("retry-after").and_then(parse_retry_after),
            body: r.into_string().unwrap_or_default(),
        },
        ureq::Error::Status(status, r) => ProviderError::Http {
            status,
            body: r.into_string().unwrap_or_default(),
        },
        other => ProviderError::Transport(other.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backoff_grows_stays_bounded_and_keeps_half_the_delay() {
        let p = RetryPolicy::default();
        for attempt in 0..10 {
            let d = p.backoff(attempt).as_millis() as u64;
            let exp = (p.base_ms << attempt).min(p.max_ms);
            assert!(d >= exp / 2 && d <= exp, "attempt {attempt}: {d}");
        }
    }

    #[test]
    fn retry_after_is_read_in_seconds() {
        assert_eq!(parse_retry_after("2"), Some(2000));
        assert_eq!(parse_retry_after("0.5"), Some(500));
        assert_eq!(parse_retry_after("Wed, 21 Oct 2015 07:28:00 GMT"), None);
    }
}
