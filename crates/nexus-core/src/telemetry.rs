//! Tracing initialisation, secret redaction, and latency histograms.
//!
//! Latency and bandwidth are first-class features (spec §0.3), so percentile tracking lives
//! in the foundation crate and is used by the media pipeline, the benchmark harness and the
//! in-app telemetry overlay alike.

use std::fmt;
use std::sync::Arc;
use std::time::Duration;

use hdrhistogram::Histogram;
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use tracing_appender::non_blocking::WorkerGuard;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;
use tracing_subscriber::{EnvFilter, Layer, Registry};

use crate::config::{LogConfig, LogFormat};

/// Keeps background log writers alive. Drop it only at process exit.
#[derive(Debug)]
pub struct TelemetryGuard {
    _file: Option<WorkerGuard>,
}

/// Initialises the global `tracing` subscriber.
///
/// `RUST_LOG`, when set, takes precedence over `cfg.level`. Calling this twice is harmless:
/// the second call leaves the first subscriber in place.
pub fn init_tracing(cfg: &LogConfig) -> TelemetryGuard {
    let filter = EnvFilter::try_from_default_env()
        .or_else(|_| EnvFilter::try_new(&cfg.level))
        .unwrap_or_else(|_| EnvFilter::new("info"));

    // All layers are boxed against `Registry` and applied as one `Vec`, which keeps the
    // composition type independent of the chosen format / file sink.
    let mut layers: Vec<Box<dyn Layer<Registry> + Send + Sync>> = Vec::with_capacity(2);
    layers.push(match cfg.format {
        LogFormat::Pretty => Box::new(
            tracing_subscriber::fmt::layer()
                .pretty()
                .with_ansi(cfg.ansi),
        ),
        LogFormat::Compact => Box::new(
            tracing_subscriber::fmt::layer()
                .compact()
                .with_ansi(cfg.ansi),
        ),
        LogFormat::Json => Box::new(tracing_subscriber::fmt::layer().json()),
    });

    let mut guard = None;
    if let Some(dir) = &cfg.file_dir {
        let appender = tracing_appender::rolling::daily(dir, "nexus.log");
        let (writer, worker) = tracing_appender::non_blocking(appender);
        layers.push(Box::new(
            tracing_subscriber::fmt::layer()
                .json()
                .with_ansi(false)
                .with_writer(writer),
        ));
        guard = Some(worker);
    }

    let _ = tracing_subscriber::registry()
        .with(layers)
        .with(filter)
        .try_init();

    TelemetryGuard { _file: guard }
}

/// Wrapper that hides a value from `Debug`/`Display`.
///
/// Wrap anything secret (tokens, keys, passphrases, clipboard contents, typed text) before it
/// can reach a log macro. The wrapped value is still accessible via `.0`.
#[derive(Clone, Default, PartialEq, Eq)]
pub struct Redacted<T>(pub T);

impl<T> fmt::Debug for Redacted<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("[redacted]")
    }
}

impl<T> fmt::Display for Redacted<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("[redacted]")
    }
}

/// Percentile summary of a latency distribution. All values are microseconds.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct LatencySummary {
    /// Number of samples.
    pub count: u64,
    /// Smallest sample.
    pub min_us: u64,
    /// Arithmetic mean.
    pub mean_us: f64,
    /// Median.
    pub p50_us: u64,
    /// 95th percentile.
    pub p95_us: u64,
    /// 99th percentile.
    pub p99_us: u64,
    /// Largest sample.
    pub max_us: u64,
}

impl fmt::Display for LatencySummary {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "n={} min={:.2}ms mean={:.2}ms p50={:.2}ms p95={:.2}ms p99={:.2}ms max={:.2}ms",
            self.count,
            ms(self.min_us),
            self.mean_us / 1000.0,
            ms(self.p50_us),
            ms(self.p95_us),
            ms(self.p99_us),
            ms(self.max_us),
        )
    }
}

#[allow(clippy::cast_precision_loss)]
fn ms(us: u64) -> f64 {
    us as f64 / 1000.0
}

/// HDR histogram of latencies with microsecond resolution up to 60 s.
///
/// The histogram is held in an `Option` only so construction can be panic-free: the bounds
/// are compile-time constants that `hdrhistogram` always accepts (asserted by a unit test),
/// and if creation ever did fail we would degrade to "no statistics" rather than crash an
/// agent that may be controlling someone's machine.
#[derive(Debug, Clone)]
pub struct LatencyStats {
    hist: Option<Histogram<u64>>,
}

impl Default for LatencyStats {
    fn default() -> Self {
        Self::new()
    }
}

impl LatencyStats {
    const MAX_US: u64 = 60_000_000;

    /// Creates an empty histogram.
    #[must_use]
    pub fn new() -> Self {
        Self {
            hist: Histogram::new_with_bounds(1, Self::MAX_US, 3).ok(),
        }
    }

    /// Records a duration (saturating at 60 s).
    pub fn record(&mut self, d: Duration) {
        self.record_us(u64::try_from(d.as_micros()).unwrap_or(u64::MAX));
    }

    /// Records a latency in microseconds (saturating at 60 s, floor of 1 µs).
    pub fn record_us(&mut self, us: u64) {
        if let Some(h) = &mut self.hist {
            h.saturating_record(us.max(1));
        }
    }

    /// Number of recorded samples.
    #[must_use]
    pub fn len(&self) -> u64 {
        self.hist.as_ref().map_or(0, Histogram::len)
    }

    /// True when nothing has been recorded.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Merges another histogram into this one.
    pub fn merge(&mut self, other: &Self) {
        if let (Some(mine), Some(theirs)) = (&mut self.hist, &other.hist) {
            // `add` only fails if `theirs` holds values beyond our range, which cannot happen
            // because both use identical bounds.
            let _ = mine.add(theirs);
        }
    }

    /// Clears all samples.
    pub fn reset(&mut self) {
        if let Some(h) = &mut self.hist {
            h.reset();
        }
    }

    /// Computes the percentile summary. An empty histogram yields zeros.
    #[must_use]
    pub fn summary(&self) -> LatencySummary {
        match &self.hist {
            Some(h) if !h.is_empty() => LatencySummary {
                count: h.len(),
                min_us: h.min(),
                mean_us: h.mean(),
                p50_us: h.value_at_quantile(0.50),
                p95_us: h.value_at_quantile(0.95),
                p99_us: h.value_at_quantile(0.99),
                max_us: h.max(),
            },
            _ => LatencySummary {
                count: 0,
                min_us: 0,
                mean_us: 0.0,
                p50_us: 0,
                p95_us: 0,
                p99_us: 0,
                max_us: 0,
            },
        }
    }
}

/// Cheap-to-clone, thread-safe handle for recording latencies from many tasks.
#[derive(Debug, Clone, Default)]
pub struct LatencyRecorder {
    inner: Arc<Mutex<LatencyStats>>,
}

impl LatencyRecorder {
    /// Creates an empty recorder.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Records a duration.
    pub fn record(&self, d: Duration) {
        self.inner.lock().record(d);
    }

    /// Records a latency in microseconds.
    pub fn record_us(&self, us: u64) {
        self.inner.lock().record_us(us);
    }

    /// Snapshot of the current distribution.
    #[must_use]
    pub fn summary(&self) -> LatencySummary {
        self.inner.lock().summary()
    }

    /// Returns the summary and clears the histogram (for windowed reporting).
    #[must_use]
    pub fn take_summary(&self) -> LatencySummary {
        let mut g = self.inner.lock();
        let s = g.summary();
        g.reset();
        s
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn redacted_never_leaks() {
        let secret = Redacted("hunter2".to_string());
        assert_eq!(format!("{secret:?}"), "[redacted]");
        assert_eq!(format!("{secret}"), "[redacted]");
        assert_eq!(secret.0, "hunter2");
    }

    #[test]
    fn constant_bounds_are_accepted_by_hdrhistogram() {
        // Guards the "creation cannot fail" assumption documented on `LatencyStats`.
        assert!(LatencyStats::new().hist.is_some());
    }

    #[test]
    fn empty_summary_is_zero() {
        let s = LatencyStats::new().summary();
        assert_eq!(s.count, 0);
        assert_eq!(s.p99_us, 0);
    }

    #[test]
    fn percentiles_of_uniform_distribution() {
        let mut stats = LatencyStats::new();
        for us in 1..=1000u64 {
            stats.record_us(us * 10); // 10 µs .. 10 ms
        }
        let s = stats.summary();
        assert_eq!(s.count, 1000);
        assert_eq!(s.min_us, 10);
        // HDR precision with 3 significant figures: within 0.1 %.
        assert!((4_990..=5_010).contains(&s.p50_us), "p50={}", s.p50_us);
        assert!((9_490..=9_510).contains(&s.p95_us), "p95={}", s.p95_us);
        assert!((9_890..=9_910).contains(&s.p99_us), "p99={}", s.p99_us);
        assert!(s.max_us >= 9_990);
    }

    #[test]
    fn records_saturate_instead_of_panicking() {
        let mut stats = LatencyStats::new();
        stats.record(Duration::from_secs(3600));
        stats.record_us(0);
        stats.record_us(u64::MAX);
        assert_eq!(stats.len(), 3);
        assert!(stats.summary().max_us <= 61_000_000);
    }

    #[test]
    fn merge_combines_counts() {
        let mut a = LatencyStats::new();
        let mut b = LatencyStats::new();
        a.record_us(100);
        b.record_us(200);
        b.record_us(300);
        a.merge(&b);
        assert_eq!(a.len(), 3);
    }

    #[test]
    fn recorder_is_shared_and_windowed() {
        let r = LatencyRecorder::new();
        let r2 = r.clone();
        r.record(Duration::from_millis(5));
        r2.record(Duration::from_millis(7));
        let first = r.take_summary();
        assert_eq!(first.count, 2);
        assert_eq!(r.summary().count, 0, "take_summary resets the window");
    }

    #[test]
    fn summary_display_is_readable() {
        let mut stats = LatencyStats::new();
        stats.record(Duration::from_millis(12));
        let text = stats.summary().to_string();
        assert!(text.contains("p95=12.0"), "{text}");
    }

    #[test]
    fn init_tracing_is_idempotent() {
        let cfg = LogConfig::default();
        let _g1 = init_tracing(&cfg);
        let _g2 = init_tracing(&cfg);
    }
}
