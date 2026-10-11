//! Monotonic microsecond clock.
//!
//! Capture, encode, send, decode and present timestamps are all expressed as
//! microseconds on a *per-process monotonic clock*. Wall-clock time is never used
//! for latency maths because it can jump (NTP, DST, manual changes).
//! Cross-device comparison is done by exchanging offsets during the session
//! handshake, not by trusting wall clocks.

use std::sync::OnceLock;
use std::time::{Duration, Instant};

static EPOCH: OnceLock<Instant> = OnceLock::new();

/// Microseconds elapsed on the process-local monotonic clock.
///
/// The first call fixes the epoch, so values are small and comfortably fit in a `u64`
/// for the lifetime of any realistic process (584,000+ years).
#[must_use]
pub fn now_us() -> u64 {
    let epoch = *EPOCH.get_or_init(Instant::now);
    // `as_micros` is u128; the truncation cannot matter within process lifetime.
    u64::try_from(epoch.elapsed().as_micros()).unwrap_or(u64::MAX)
}

/// Converts a microsecond delta into a [`Duration`].
#[must_use]
pub const fn us_to_duration(us: u64) -> Duration {
    Duration::from_micros(us)
}

/// Wall-clock milliseconds since the Unix epoch.
///
/// Only for *human-facing* or *protocol-expiry* timestamps (audit log, token expiry).
/// Returns 0 if the system clock is set before 1970.
#[must_use]
pub fn unix_ms() -> u64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn monotonic_never_goes_backwards() {
        let mut last = now_us();
        for _ in 0..10_000 {
            let n = now_us();
            assert!(n >= last);
            last = n;
        }
    }

    #[test]
    fn unix_ms_is_after_2024() {
        // 2024-01-01T00:00:00Z
        assert!(unix_ms() > 1_704_067_200_000);
    }

    #[test]
    fn conversion_roundtrip() {
        assert_eq!(us_to_duration(1_500).as_micros(), 1_500);
    }
}
