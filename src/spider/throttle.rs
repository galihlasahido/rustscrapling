//! Per-domain adaptive delays, a port of `scrapling/spiders/throttle.py`.

use std::collections::HashMap;
use std::time::SystemTime;

use crate::error::{Error, Result};
use crate::response::HeaderMap;

/// The factor a domain's delay is multiplied by when it blocks us and sends no `Retry-After`.
const BLOCK_BACKOFF_FACTOR: f64 = 2.0;

/// A delay in seconds that arithmetic can safely run on: never negative, never a NaN, never
/// an infinity. Values reach the throttle from response headers, so none of that is hypothetical.
fn sane(seconds: f64) -> f64 {
    if seconds.is_finite() && seconds > 0.0 {
        seconds
    } else {
        0.0
    }
}

/// Seconds a `Retry-After` header asks for: a number of seconds, or an HTTP date.
///
/// Returns `None` when the header is missing or unreadable; a date already in the past is
/// reported as `0.0`, never as a negative wait.
pub fn parse_retry_after(headers: &HeaderMap) -> Option<f64> {
    let value = headers.get("retry-after")?.trim().to_string();
    if value.is_empty() {
        return None;
    }

    if let Ok(seconds) = value.parse::<f64>() {
        if seconds.is_finite() {
            return Some(seconds.max(0.0));
        }
    }

    match httpdate::parse_http_date(&value) {
        Ok(when) => Some(
            when.duration_since(SystemTime::now())
                .map(|delta| delta.as_secs_f64())
                .unwrap_or(0.0),
        ),
        Err(_) => {
            tracing::debug!(header = %value, "ignoring an unreadable `Retry-After` header");
            None
        }
    }
}

/// How the throttle starts and how far it may go.
#[derive(Debug, Clone, Copy)]
#[non_exhaustive]
pub struct AutoThrottleConfig {
    /// Delay used for a domain's first request. Default 5.0s.
    pub start_delay: f64,
    /// Highest delay allowed. Default 60.0s.
    pub max_delay: f64,
    /// Requests aimed to have in flight per domain. Default 1.0.
    pub target_concurrency: f64,
    /// Double the delay (or honour `Retry-After`) when a domain blocks us. Default `true`.
    pub block_backoff: bool,
}

impl Default for AutoThrottleConfig {
    fn default() -> Self {
        AutoThrottleConfig {
            start_delay: 5.0,
            max_delay: 60.0,
            target_concurrency: 1.0,
            block_backoff: true,
        }
    }
}

/// Per-domain delay that adapts to observed latency, as Scrapling's `AutoThrottle`.
#[derive(Debug, Clone)]
pub struct AutoThrottle {
    config: AutoThrottleConfig,
    delays: HashMap<String, f64>,
}

impl AutoThrottle {
    /// Build a throttle; errors when `target_concurrency <= 0` or `max_delay < start_delay`.
    pub fn new(config: AutoThrottleConfig) -> Result<AutoThrottle> {
        // `<= 0.0` is false for a NaN, so test it explicitly: a NaN concurrency would turn
        // every computed delay into a NaN and stall the crawl.
        if config.target_concurrency.is_nan() || config.target_concurrency <= 0.0 {
            return Err(Error::Spider(
                "`target_concurrency` must be higher than 0".to_string(),
            ));
        }
        if config.start_delay.is_nan() || config.max_delay.is_nan() {
            return Err(Error::Spider(
                "`autothrottle_start_delay` and `autothrottle_max_delay` must be numbers"
                    .to_string(),
            ));
        }
        if config.start_delay < 0.0 {
            return Err(Error::Spider(
                "`autothrottle_start_delay` can't be negative".to_string(),
            ));
        }
        if config.max_delay < config.start_delay {
            return Err(Error::Spider(
                "`autothrottle_max_delay` can't be lower than `autothrottle_start_delay`"
                    .to_string(),
            ));
        }
        Ok(AutoThrottle {
            config,
            delays: HashMap::new(),
        })
    }

    /// The configuration this throttle was built with.
    pub fn config(&self) -> AutoThrottleConfig {
        self.config
    }

    /// The current delay for a domain, seeded from `start_delay` the first time.
    pub fn delay_for(&mut self, domain: &str, floor: f64) -> f64 {
        if let Some(delay) = self.delays.get(domain) {
            return *delay;
        }
        let seeded = sane(floor)
            .max(self.config.start_delay)
            .min(self.config.max_delay);
        self.delays.insert(domain.to_string(), seeded);
        seeded
    }

    /// Feed a finished request back in and return the domain's new delay.
    ///
    /// The formula is Python's: the delay converges on `latency / target_concurrency`, and a
    /// block can only ever slow the spider down.
    pub fn record(
        &mut self,
        domain: &str,
        latency: f64,
        ok: bool,
        floor: f64,
        retry_after: Option<f64>,
    ) -> f64 {
        let floor = sane(floor);
        let current = self.delay_for(domain, floor);
        let target = sane(latency) / self.config.target_concurrency;
        let mut new_delay = ((current + target) / 2.0).max(target);

        if !ok {
            let penalty = if self.config.block_backoff {
                // The `Retry-After` behind this comes from the server, so keep it finite.
                retry_after
                    .map(sane)
                    .unwrap_or(current * BLOCK_BACKOFF_FACTOR)
            } else {
                current
            };
            new_delay = new_delay.max(penalty).max(current);
        }

        new_delay = new_delay.max(floor).min(self.config.max_delay);
        self.delays.insert(domain.to_string(), new_delay);
        tracing::debug!(
            domain,
            latency,
            ok,
            from = current,
            to = new_delay,
            "autothrottle adjusted a domain delay"
        );
        new_delay
    }

    /// Forget every learned delay.
    pub fn reset(&mut self) {
        self.delays.clear();
    }

    /// A snapshot of the learned delays, for the stats.
    pub fn delays(&self) -> HashMap<String, f64> {
        self.delays.clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn close(left: f64, right: f64) {
        assert!((left - right).abs() < 1e-9, "{left} != {right}");
    }

    #[test]
    fn rejects_impossible_configurations() {
        let config = AutoThrottleConfig {
            target_concurrency: 0.0,
            ..AutoThrottleConfig::default()
        };
        assert!(AutoThrottle::new(config).is_err());

        let config = AutoThrottleConfig {
            start_delay: 10.0,
            max_delay: 5.0,
            ..AutoThrottleConfig::default()
        };
        assert!(AutoThrottle::new(config).is_err());
    }

    #[test]
    fn first_delay_is_the_start_delay_clamped_by_the_floor() {
        let mut throttle = AutoThrottle::new(AutoThrottleConfig::default()).expect("config");
        close(throttle.delay_for("example.com", 0.0), 5.0);
        close(throttle.delay_for("other.com", 8.0), 8.0);
    }

    #[test]
    fn record_converges_on_latency_over_target_concurrency() {
        let mut throttle = AutoThrottle::new(AutoThrottleConfig::default()).expect("config");
        // current = 5.0, target = 1.0 -> max((5 + 1) / 2, 1) = 3.0
        close(throttle.record("example.com", 1.0, true, 0.0, None), 3.0);
        // current = 3.0, target = 1.0 -> max(2.0, 1.0) = 2.0
        close(throttle.record("example.com", 1.0, true, 0.0, None), 2.0);
        close(throttle.record("example.com", 1.0, true, 0.0, None), 1.5);
    }

    #[test]
    fn a_slow_response_raises_the_delay_immediately() {
        let mut throttle = AutoThrottle::new(AutoThrottleConfig::default()).expect("config");
        // current = 5.0, target = 20.0 -> max(12.5, 20.0) = 20.0
        close(throttle.record("example.com", 20.0, true, 0.0, None), 20.0);
    }

    #[test]
    fn a_block_doubles_the_delay() {
        let mut throttle = AutoThrottle::new(AutoThrottleConfig::default()).expect("config");
        // current = 5.0, target = 1.0 -> 3.0, then the penalty of 10.0 wins.
        close(throttle.record("example.com", 1.0, false, 0.0, None), 10.0);
    }

    #[test]
    fn a_block_honours_retry_after() {
        let mut throttle = AutoThrottle::new(AutoThrottleConfig::default()).expect("config");
        close(
            throttle.record("example.com", 1.0, false, 0.0, Some(30.0)),
            30.0,
        );
    }

    #[test]
    fn a_block_never_speeds_the_spider_up() {
        let mut throttle = AutoThrottle::new(AutoThrottleConfig::default()).expect("config");
        // Retry-After of 0 must not drop below the current delay.
        close(
            throttle.record("example.com", 1.0, false, 0.0, Some(0.0)),
            5.0,
        );
    }

    #[test]
    fn without_backoff_a_block_only_holds_the_current_delay() {
        let config = AutoThrottleConfig {
            block_backoff: false,
            ..AutoThrottleConfig::default()
        };
        let mut throttle = AutoThrottle::new(config).expect("config");
        close(
            throttle.record("example.com", 1.0, false, 0.0, Some(30.0)),
            5.0,
        );
    }

    #[test]
    fn the_delay_is_clamped_by_the_floor_and_the_maximum() {
        let config = AutoThrottleConfig {
            max_delay: 8.0,
            ..AutoThrottleConfig::default()
        };
        let mut throttle = AutoThrottle::new(config).expect("config");
        close(throttle.record("example.com", 100.0, true, 0.0, None), 8.0);

        let mut throttle = AutoThrottle::new(AutoThrottleConfig::default()).expect("config");
        close(throttle.record("example.com", 0.1, true, 4.0, None), 4.0);
    }

    #[test]
    fn reset_forgets_every_domain() {
        let mut throttle = AutoThrottle::new(AutoThrottleConfig::default()).expect("config");
        throttle.record("example.com", 1.0, true, 0.0, None);
        assert_eq!(throttle.delays().len(), 1);
        throttle.reset();
        assert!(throttle.delays().is_empty());
        close(throttle.delay_for("example.com", 0.0), 5.0);
    }

    #[test]
    fn retry_after_reads_seconds() {
        let headers = HeaderMap::from_pairs([("Retry-After".to_string(), "120".to_string())]);
        assert_eq!(parse_retry_after(&headers), Some(120.0));
    }

    #[test]
    fn retry_after_clamps_negatives_and_ignores_junk() {
        let headers = HeaderMap::from_pairs([("retry-after".to_string(), "-5".to_string())]);
        assert_eq!(parse_retry_after(&headers), Some(0.0));

        let headers = HeaderMap::from_pairs([("retry-after".to_string(), "soon".to_string())]);
        assert_eq!(parse_retry_after(&headers), None);

        assert_eq!(parse_retry_after(&HeaderMap::new()), None);
    }

    #[test]
    fn retry_after_reads_an_http_date_in_the_past_as_zero() {
        let headers = HeaderMap::from_pairs([(
            "Retry-After".to_string(),
            "Wed, 21 Oct 2015 07:28:00 GMT".to_string(),
        )]);
        assert_eq!(parse_retry_after(&headers), Some(0.0));
    }
}
