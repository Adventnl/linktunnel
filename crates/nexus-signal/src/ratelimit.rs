//! Token-bucket rate limiting, per key.

use std::collections::HashMap;
use std::hash::Hash;
use std::time::Instant;

use crate::config::Rate;

#[derive(Debug, Clone, Copy)]
struct Bucket {
    tokens: f64,
    last: Instant,
}

/// Limits actions per key (an IP, a device, …). Memory is bounded: when the table is full the
/// idle entries (full buckets) are dropped first, which loses nothing.
#[derive(Debug)]
pub struct KeyedLimiter<K> {
    rate: Rate,
    buckets: HashMap<K, Bucket>,
    max_keys: usize,
}

impl<K: Hash + Eq + Clone> KeyedLimiter<K> {
    /// A limiter allowing `rate` per key, tracking at most `max_keys` keys.
    #[must_use]
    pub fn new(rate: Rate, max_keys: usize) -> Self {
        Self {
            rate,
            buckets: HashMap::new(),
            max_keys: max_keys.max(1),
        }
    }

    fn refill(&self, bucket: &mut Bucket, now: Instant) {
        let elapsed = now.saturating_duration_since(bucket.last).as_secs_f64();
        bucket.tokens =
            (bucket.tokens + elapsed * self.rate.per_sec).min(f64::from(self.rate.burst));
        bucket.last = now;
    }

    /// Takes one token for `key` at time `now`. Returns `Err(wait)` with the time until a token
    /// is available when the key is over its rate.
    pub fn check_at(&mut self, key: &K, now: Instant) -> Result<(), std::time::Duration> {
        if self.buckets.len() >= self.max_keys && !self.buckets.contains_key(key) {
            let burst = f64::from(self.rate.burst);
            let rate = self.rate;
            self.buckets.retain(|_, b| {
                let elapsed = now.saturating_duration_since(b.last).as_secs_f64();
                (b.tokens + elapsed * rate.per_sec) < burst
            });
            if self.buckets.len() >= self.max_keys {
                // Every tracked key is actively limited; refuse new keys rather than grow.
                return Err(std::time::Duration::from_secs(1));
            }
        }
        let initial = Bucket {
            tokens: f64::from(self.rate.burst),
            last: now,
        };
        let mut bucket = *self.buckets.get(key).unwrap_or(&initial);
        self.refill(&mut bucket, now);
        let result = if bucket.tokens >= 1.0 {
            bucket.tokens -= 1.0;
            Ok(())
        } else {
            let missing = 1.0 - bucket.tokens;
            let wait = if self.rate.per_sec > 0.0 {
                missing / self.rate.per_sec
            } else {
                60.0
            };
            Err(std::time::Duration::from_secs_f64(wait))
        };
        self.buckets.insert(key.clone(), bucket);
        result
    }

    /// Whether `key` could take a token right now, without taking one.
    #[must_use]
    pub fn would_allow(&self, key: &K) -> bool {
        let now = Instant::now();
        match self.buckets.get(key) {
            None => true,
            Some(b) => {
                let elapsed = now.saturating_duration_since(b.last).as_secs_f64();
                b.tokens + elapsed * self.rate.per_sec >= 1.0
            }
        }
    }

    /// [`check_at`](Self::check_at) using the current time.
    pub fn check(&mut self, key: &K) -> Result<(), std::time::Duration> {
        self.check_at(key, Instant::now())
    }

    /// Number of keys currently tracked.
    #[must_use]
    pub fn tracked(&self) -> usize {
        self.buckets.len()
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;

    fn limiter(per_sec: f64, burst: u32) -> KeyedLimiter<&'static str> {
        KeyedLimiter::new(Rate { per_sec, burst }, 1000)
    }

    #[test]
    fn allows_a_burst_then_throttles() {
        let mut l = limiter(1.0, 3);
        let t0 = Instant::now();
        for _ in 0..3 {
            assert!(l.check_at(&"k", t0).is_ok());
        }
        let wait = l.check_at(&"k", t0).unwrap_err();
        assert!(
            wait > Duration::from_millis(900) && wait <= Duration::from_secs(1),
            "{wait:?}"
        );
    }

    #[test]
    fn refills_over_time() {
        let mut l = limiter(2.0, 2);
        let t0 = Instant::now();
        l.check_at(&"k", t0).unwrap();
        l.check_at(&"k", t0).unwrap();
        assert!(l.check_at(&"k", t0).is_err());
        assert!(
            l.check_at(&"k", t0 + Duration::from_millis(500)).is_ok(),
            "one token after 0.5 s"
        );
        assert!(l.check_at(&"k", t0 + Duration::from_millis(500)).is_err());
        // Never exceeds the burst however long we wait.
        let later = t0 + Duration::from_secs(3600);
        l.check_at(&"k", later).unwrap();
        l.check_at(&"k", later).unwrap();
        assert!(l.check_at(&"k", later).is_err());
    }

    #[test]
    fn keys_are_independent() {
        let mut l = limiter(0.0, 1);
        let t0 = Instant::now();
        assert!(l.check_at(&"a", t0).is_ok());
        assert!(l.check_at(&"a", t0).is_err());
        assert!(l.check_at(&"b", t0).is_ok());
    }

    #[test]
    fn would_allow_does_not_consume() {
        let mut l = limiter(0.0, 1);
        assert!(l.would_allow(&"k"));
        assert!(l.would_allow(&"k"));
        l.check(&"k").unwrap();
        assert!(!l.would_allow(&"k"));
    }

    #[test]
    fn memory_is_bounded_and_idle_keys_are_evicted_first() {
        let mut l: KeyedLimiter<u32> = KeyedLimiter::new(
            Rate {
                per_sec: 1000.0,
                burst: 5,
            },
            10,
        );
        let t0 = Instant::now();
        for k in 0..10 {
            l.check_at(&k, t0).unwrap();
        }
        // After a pause every bucket has refilled (is idle), so ten new keys evict the old ones.
        let later = t0 + Duration::from_secs(1);
        for k in 100..110 {
            l.check_at(&k, later).unwrap();
        }
        assert!(l.tracked() <= 10, "{}", l.tracked());
        // Those ten are now *active* (just used), so an eleventh key is refused rather than
        // growing the table: see `a_flood_of_active_keys_cannot_grow_the_table`.
        assert!(l.check_at(&110, later).is_err());
        // Once they too have idled, there is room again.
        l.check_at(&110, later + Duration::from_secs(1)).unwrap();
    }

    #[test]
    fn a_flood_of_active_keys_cannot_grow_the_table() {
        let mut l: KeyedLimiter<u32> = KeyedLimiter::new(
            Rate {
                per_sec: 0.001,
                burst: 1,
            },
            4,
        );
        let t0 = Instant::now();
        let mut refused = 0;
        for k in 0..100 {
            if l.check_at(&k, t0).is_err() {
                refused += 1;
            }
        }
        assert!(l.tracked() <= 4);
        assert!(
            refused > 90,
            "table-exhaustion attempts must be refused, got {refused}"
        );
    }
}
