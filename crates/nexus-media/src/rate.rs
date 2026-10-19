//! Receiver-side bitrate control: a delay-gradient (GCC-style) overuse detector plus AIMD.
//!
//! The controller watches how video frames arrive. If frames take progressively *longer* to
//! arrive than they took to send, a queue is building somewhere on the path (bufferbloat, a
//! shared link, a relay) and the sender is going faster than the path can carry. That signal
//! arrives one round trip *before* any loss, which is what lets us hold latency down instead of
//! filling buffers and then reacting to drops.
//!
//! * **Detector:** for every frame, `d = Δarrival − Δsend`. The accumulated, smoothed `d` is
//!   regressed over a sliding window; a persistently positive slope above an adaptive threshold
//!   means *overuse*, a negative one *underuse*, otherwise *normal*.
//! * **Controller:** overuse → drop to `β ×` the measured goodput; normal → probe upward
//!   (multiplicatively far from the last known capacity, gently near it); underuse → hold while
//!   the queue drains. Reported loss caps the rate too.
//!
//! Everything is pure and takes time as an argument, so it is tested deterministically against
//! simulated links (`#[cfg(test)]`), including step changes and jitter.

use std::collections::VecDeque;

/// Bitrate limits.
#[derive(Debug, Clone, Copy)]
pub struct RateConfig {
    /// Never go below this (bits per second).
    pub min_bps: u32,
    /// Never go above this.
    pub max_bps: u32,
    /// Where to start.
    pub start_bps: u32,
}

impl Default for RateConfig {
    fn default() -> Self {
        Self {
            min_bps: 300_000,
            max_bps: 50_000_000,
            start_bps: 4_000_000,
        }
    }
}

/// One fully received frame.
#[derive(Debug, Clone, Copy)]
pub struct FrameArrival {
    /// Sender clock (µs) when the frame was handed to the network.
    pub send_us: u64,
    /// Receiver clock (µs) when its last byte arrived.
    pub recv_us: u64,
    /// Compressed size.
    pub bytes: usize,
    /// A keyframe: far larger than its neighbours by nature, so its late arrival says nothing
    /// about the path and must not be read as congestion.
    pub is_key: bool,
}

/// What the detector concluded.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Usage {
    /// The path is keeping up.
    Normal,
    /// A queue is building: send less.
    Overuse,
    /// A queue is draining: hold.
    Underuse,
}

const WINDOW: usize = 20;
const SMOOTHING: f64 = 0.9;
const THRESHOLD_GAIN: f64 = 4.0;
const THRESHOLD_MIN: f64 = 6.0;
const THRESHOLD_MAX: f64 = 600.0;
/// A frame this much slower than the best recent one means a standing queue (ms).
const QUEUE_DELAY_LIMIT_MS: f64 = 80.0;
/// Overuse must persist this long (ms) before it is believed.
const OVERUSE_CONFIRM_MS: f64 = 10.0;

/// The trendline overuse detector.
#[derive(Debug)]
pub struct TrendDetector {
    last: Option<(u64, u64)>,
    accumulated: f64,
    smoothed: f64,
    deltas: u32,
    window: VecDeque<(f64, f64)>,
    first_arrival_ms: Option<f64>,
    threshold: f64,
    overuse_since: Option<f64>,
    prev_trend: f64,
    last_update_ms: Option<f64>,
    usage: Usage,
}

impl Default for TrendDetector {
    fn default() -> Self {
        Self::new()
    }
}

impl TrendDetector {
    /// Creates a detector in the `Normal` state.
    #[must_use]
    pub fn new() -> Self {
        Self {
            last: None,
            accumulated: 0.0,
            smoothed: 0.0,
            deltas: 0,
            window: VecDeque::with_capacity(WINDOW + 1),
            first_arrival_ms: None,
            threshold: 12.5,
            overuse_since: None,
            prev_trend: 0.0,
            last_update_ms: None,
            usage: Usage::Normal,
        }
    }

    /// Current verdict.
    #[must_use]
    pub fn usage(&self) -> Usage {
        self.usage
    }

    /// Steps over a frame without analysing it (keyframes), keeping the send/arrival baseline so
    /// the next delta is measured against the right predecessor.
    pub fn skip(&mut self, a: FrameArrival) {
        self.last = Some((a.send_us, a.recv_us));
    }

    /// Feeds one frame arrival and returns the updated verdict.
    #[allow(clippy::cast_precision_loss)]
    pub fn update(&mut self, a: FrameArrival) -> Usage {
        let Some((prev_send, prev_recv)) = self.last.replace((a.send_us, a.recv_us)) else {
            return self.usage;
        };
        let send_delta_ms = a.send_us.wrapping_sub(prev_send) as i64 as f64 / 1000.0;
        let recv_delta_ms = a.recv_us.wrapping_sub(prev_recv) as i64 as f64 / 1000.0;
        let delta = recv_delta_ms - send_delta_ms;
        let now_ms = a.recv_us as f64 / 1000.0;

        self.deltas = (self.deltas + 1).min(1000);
        self.accumulated += delta;
        self.smoothed = SMOOTHING * self.smoothed + (1.0 - SMOOTHING) * self.accumulated;
        let first = *self.first_arrival_ms.get_or_insert(now_ms);
        self.window.push_back((now_ms - first, self.smoothed));
        if self.window.len() > WINDOW {
            self.window.pop_front();
        }
        let trend = if self.window.len() == WINDOW {
            slope(&self.window)
        } else {
            self.prev_trend
        };
        self.detect(trend, delta, now_ms);
        self.usage
    }

    fn detect(&mut self, trend: f64, delta: f64, now_ms: f64) {
        let modified = f64::from(self.deltas.min(60)) * trend * THRESHOLD_GAIN;
        if modified > self.threshold {
            let since = *self.overuse_since.get_or_insert(now_ms);
            // Believe it once it has lasted a moment and the delay is still growing.
            if now_ms - since >= OVERUSE_CONFIRM_MS && trend >= self.prev_trend && delta > 0.0 {
                self.usage = Usage::Overuse;
                self.overuse_since = None;
            }
        } else if modified < -self.threshold {
            self.usage = Usage::Underuse;
            self.overuse_since = None;
        } else {
            self.usage = Usage::Normal;
            self.overuse_since = None;
        }
        self.prev_trend = trend;
        // The threshold adapts so a noisy-but-stable path does not trip it, and a quiet one stays
        // sensitive (it moves faster up than down).
        if let Some(last) = self.last_update_ms {
            let k = if modified.abs() < self.threshold {
                0.039
            } else {
                0.0087
            };
            let dt = (now_ms - last).min(100.0);
            self.threshold = (self.threshold + k * (modified.abs() - self.threshold) * dt)
                .clamp(THRESHOLD_MIN, THRESHOLD_MAX);
        }
        self.last_update_ms = Some(now_ms);
    }
}

/// Least-squares slope of y over x.
fn slope(points: &VecDeque<(f64, f64)>) -> f64 {
    #[allow(clippy::cast_precision_loss)]
    let n = points.len() as f64;
    let (sx, sy) = points
        .iter()
        .fold((0.0, 0.0), |(sx, sy), (x, y)| (sx + x, sy + y));
    let (mx, my) = (sx / n, sy / n);
    let (num, den) = points.iter().fold((0.0, 0.0), |(num, den), (x, y)| {
        (num + (x - mx) * (y - my), den + (x - mx) * (x - mx))
    });
    if den.abs() < f64::EPSILON {
        0.0
    } else {
        num / den
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Phase {
    Increase,
    Hold,
}

/// Measures goodput over a sliding window.
#[derive(Debug, Default)]
struct Goodput {
    samples: VecDeque<(f64, usize)>,
}

impl Goodput {
    const WINDOW_MS: f64 = 500.0;

    fn add(&mut self, now_ms: f64, bytes: usize) {
        self.samples.push_back((now_ms, bytes));
        while self
            .samples
            .front()
            .is_some_and(|(t, _)| now_ms - t > Self::WINDOW_MS)
        {
            self.samples.pop_front();
        }
    }

    /// Bits per second over the whole window, if there is enough data to say.
    fn bps(&self) -> Option<f64> {
        self.bps_over(Self::WINDOW_MS)
    }

    /// Bits per second over the most recent `window_ms`. A short window tracks a sudden capacity
    /// change; the long one smooths over bursty frame sizes.
    #[allow(clippy::cast_precision_loss)]
    fn bps_over(&self, window_ms: f64) -> Option<f64> {
        let (last, _) = self.samples.back()?;
        let recent: Vec<&(f64, usize)> = self
            .samples
            .iter()
            .filter(|(t, _)| last - t <= window_ms)
            .collect();
        if recent.len() < 2 {
            return None;
        }
        let first = recent.first()?.0;
        let span = (last - first).max(20.0);
        // The first sample only marks the start of the interval.
        let bytes: usize = recent.iter().skip(1).map(|(_, b)| *b).sum();
        Some(bytes as f64 * 8.0 / (span / 1000.0))
    }
}

/// The bitrate controller. Feed it arrivals; ask it what to tell the sender.
#[derive(Debug)]
pub struct RateController {
    cfg: RateConfig,
    detector: TrendDetector,
    target: f64,
    goodput: Goodput,
    phase: Phase,
    hold_until_ms: f64,
    capacity_bps: Option<f64>,
    last_change_ms: f64,
    last_increase_ms: f64,
    announced: u32,
    /// Smallest one-way delay seen recently, µs (the "empty queue" baseline).
    owd_floor: VecDeque<(f64, i64)>,
    /// Since when frames have been arriving without queueing delay (ms), if they are.
    queue_empty_since_ms: Option<f64>,
    /// Per-frame QUIC streams may complete out of send order.
    newest_send_us: Option<u64>,
}

impl RateController {
    /// Creates a controller at `cfg.start_bps`.
    #[must_use]
    pub fn new(cfg: RateConfig) -> Self {
        Self {
            cfg,
            detector: TrendDetector::new(),
            target: f64::from(cfg.start_bps),
            goodput: Goodput::default(),
            phase: Phase::Increase,
            hold_until_ms: 0.0,
            capacity_bps: None,
            last_change_ms: 0.0,
            last_increase_ms: 0.0,
            announced: cfg.start_bps,
            owd_floor: VecDeque::new(),
            queue_empty_since_ms: None,
            newest_send_us: None,
        }
    }

    /// The current target in bits per second.
    #[must_use]
    pub fn target_bps(&self) -> u32 {
        self.clamp(self.target)
    }

    /// The detector's current verdict (for the HUD and tests).
    #[must_use]
    pub fn usage(&self) -> Usage {
        self.detector.usage()
    }

    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    fn clamp(&self, v: f64) -> u32 {
        v.clamp(f64::from(self.cfg.min_bps), f64::from(self.cfg.max_bps)) as u32
    }

    /// Reports a received frame.
    #[allow(clippy::cast_precision_loss)]
    pub fn on_frame(&mut self, a: FrameArrival) {
        let now_ms = a.recv_us as f64 / 1000.0;
        self.goodput.add(now_ms, a.bytes);
        // A retransmitted old stream can finish after newer ones. Its delay is real, but it
        // says nothing about a growing queue for new traffic. Do not move the baseline back.
        if self.newest_send_us.is_some_and(|send| a.send_us <= send) {
            return;
        }
        self.newest_send_us = Some(a.send_us);
        if a.is_key {
            // Counted as delivered data, but excluded from delay analysis.
            self.detector.skip(a);
            return;
        }
        // Fast path: a frame that took far longer than the best recent frame has been sitting in
        // a queue. That is unambiguous and shows up within a frame or two, much earlier than the
        // trend over a window of frames.
        #[allow(clippy::cast_possible_wrap)]
        let owd = a.recv_us as i64 - a.send_us as i64;
        self.owd_floor.push_back((now_ms, owd));
        while self
            .owd_floor
            .front()
            .is_some_and(|(t, _)| now_ms - t > 5000.0)
        {
            self.owd_floor.pop_front();
        }
        let floor = self.owd_floor.iter().map(|(_, o)| *o).min().unwrap_or(owd);
        let queue_ms = (owd - floor) as f64 / 1000.0;
        if queue_ms < 15.0 {
            self.queue_empty_since_ms.get_or_insert(now_ms);
        } else {
            self.queue_empty_since_ms = None;
        }
        if queue_ms > QUEUE_DELAY_LIMIT_MS && now_ms >= self.hold_until_ms {
            self.on_overuse(now_ms);
            self.increase(now_ms);
            return;
        }
        match self.detector.update(a) {
            Usage::Overuse => {
                // One reaction per hold period: the detector keeps saying "overuse" for as long as
                // the old queue drains, and each reaction must be judged on its own.
                if now_ms >= self.hold_until_ms {
                    self.on_overuse(now_ms);
                }
            }
            Usage::Underuse => {
                // Hold while a queue drains, but only once per episode: re-arming on every frame
                // would hold forever, since "draining" can persist long after the queue is gone.
                if self.phase != Phase::Hold && queue_ms > 15.0 {
                    self.phase = Phase::Hold;
                    self.hold_until_ms = self.hold_until_ms.max(now_ms + 100.0);
                }
            }
            Usage::Normal => {
                if self.phase == Phase::Hold && now_ms >= self.hold_until_ms {
                    self.phase = Phase::Increase;
                }
            }
        }
        self.increase(now_ms);
    }

    fn on_overuse(&mut self, now_ms: f64) {
        // Back off to a fraction of what actually got through, not of what we asked for. Few
        // frames per second (an encoder holding a tight rate on hard content) leave little to
        // measure in a short window, so widen it rather than guess.
        let recent = self.goodput.bps_over(300.0);
        let acked = recent
            .or_else(|| self.goodput.bps_over(1000.0))
            .unwrap_or(self.target);
        let mut reduced = (acked * 0.85).min(self.target * 0.95);
        if recent.is_none() {
            // The estimate is a guess from a thin window: do not cut by more than 40 % on it.
            reduced = reduced.max(self.target * 0.6);
        }
        self.target = reduced.max(f64::from(self.cfg.min_bps));
        self.capacity_bps = Some(acked);
        self.phase = Phase::Hold;
        self.hold_until_ms = now_ms + 300.0;
        self.last_change_ms = now_ms;
    }

    fn increase(&mut self, now_ms: f64) {
        // A path that has shown no queueing for a while can take more, whatever the trend filter
        // still remembers.
        let queue_clear = self
            .queue_empty_since_ms
            .is_some_and(|t| now_ms - t >= 300.0);
        if (self.phase != Phase::Increase && !queue_clear) || now_ms < self.hold_until_ms {
            return;
        }
        // Increase on a 100 ms cadence.
        if now_ms - self.last_increase_ms < 100.0 {
            return;
        }
        let acked = self.goodput.bps();
        // Native-resolution desktop mode may send only 1–5 fps. Preserve the 100 ms
        // probing pace at those rates, but cap catch-up after an idle desktop at one second.
        let steps = ((now_ms - self.last_increase_ms) / 100.0).clamp(1.0, 10.0);
        self.last_increase_ms = now_ms;
        let near_capacity = self.capacity_bps.is_some_and(|c| self.target > c * 0.85);
        let factor: f64 = if near_capacity { 1.01 } else { 1.06 };
        self.target *= factor.powf(steps);
        // If the sender is actually using the rate, never run far ahead of what has been shown to
        // work. If it is *not* (an encoder that undershoots its target on hard content), a higher
        // target is the only way to find out what the path can carry, and costs nothing until the
        // sender starts to fill it: at which point the delay signal takes over.
        if let Some(a) = acked
            && a >= self.target * 0.7
        {
            self.target = self.target.min(a * 1.5 + 200_000.0);
        }
        self.target = f64::from(self.clamp(self.target));
    }

    /// Reports frame loss over a window (frames expected vs frames that never arrived).
    pub fn on_loss(&mut self, lost: u32, expected: u32, now_ms: f64) {
        if expected == 0 {
            return;
        }
        let p = f64::from(lost) / f64::from(expected);
        if p > 0.10 {
            self.target = (self.target * (1.0 - 0.5 * p)).max(f64::from(self.cfg.min_bps));
            self.phase = Phase::Hold;
            self.hold_until_ms = now_ms + 500.0;
            self.last_change_ms = now_ms;
        }
    }

    /// The new target if it changed enough (≥ 5 %, or any decrease ≥ 2 %) since last announced.
    pub fn poll(&mut self) -> Option<u32> {
        let now = self.target_bps();
        let prev = f64::from(self.announced);
        let ratio = f64::from(now) / prev;
        if ratio >= 1.05 || ratio <= 0.98 {
            self.announced = now;
            Some(now)
        } else {
            None
        }
    }
}

#[cfg(test)]
#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::cast_precision_loss
)]
mod tests {
    use super::*;

    /// A simulated path: FIFO queue draining at `capacity`, plus propagation delay and jitter.
    struct Link {
        capacity_bps: f64,
        prop_us: u64,
        free_at_us: f64,
        jitter_us: u64,
        rng: u64,
        queue_cap_bytes: f64,
        pub max_queue_ms: f64,
    }

    impl Link {
        fn new(capacity_bps: f64) -> Self {
            Self {
                capacity_bps,
                prop_us: 20_000,
                free_at_us: 0.0,
                jitter_us: 0,
                rng: 0x9E37_79B9_7F4A_7C15,
                queue_cap_bytes: 1e9,
                max_queue_ms: 0.0,
            }
        }

        fn rand(&mut self) -> f64 {
            self.rng ^= self.rng << 13;
            self.rng ^= self.rng >> 7;
            self.rng ^= self.rng << 17;
            #[allow(clippy::cast_precision_loss)]
            {
                (self.rng % 1_000_000) as f64 / 1_000_000.0
            }
        }

        /// Returns the arrival time of a frame sent at `send_us`, or `None` if the queue dropped it.
        #[allow(
            clippy::cast_precision_loss,
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss
        )]
        fn send(&mut self, send_us: u64, bytes: usize) -> Option<u64> {
            let start = self.free_at_us.max(send_us as f64);
            let queued_bytes = (start - send_us as f64) / 1e6 * self.capacity_bps / 8.0;
            if queued_bytes > self.queue_cap_bytes {
                return None;
            }
            self.max_queue_ms = self.max_queue_ms.max((start - send_us as f64) / 1000.0);
            let tx = bytes as f64 * 8.0 / self.capacity_bps * 1e6;
            self.free_at_us = start + tx;
            let jitter = if self.jitter_us == 0 {
                0.0
            } else {
                (self.rand() - 0.5) * 2.0 * self.jitter_us as f64
            };
            Some((self.free_at_us + self.prop_us as f64 + jitter).max(0.0) as u64)
        }
    }

    /// Runs a sender at the controller's target (CBR, `fps` frames/s) for `secs`, with the link
    /// capacity given by `capacity_at(t_seconds)`. Returns (final target, target trace, max queue ms).
    fn simulate(
        secs: f64,
        fps: f64,
        jitter_us: u64,
        capacity_at: impl Fn(f64) -> f64,
    ) -> (u32, Vec<(f64, u32)>, f64) {
        let mut rc = RateController::new(RateConfig {
            min_bps: 200_000,
            max_bps: 100_000_000,
            start_bps: 2_000_000,
        });
        let mut link = Link::new(capacity_at(0.0));
        link.jitter_us = jitter_us;
        let mut trace = Vec::new();
        let step_us = (1e6 / fps) as u64;
        let mut t_us = 1_000_000u64;
        let mut arrivals: Vec<(u64, FrameArrival)> = Vec::new();
        let mut sender_rate = rc.target_bps();
        let end = (secs * 1e6) as u64 + 1_000_000;
        while t_us < end {
            let secs_now = (t_us - 1_000_000) as f64 / 1e6;
            link.capacity_bps = capacity_at(secs_now);
            let bytes = (f64::from(sender_rate) / 8.0 / fps) as usize;
            if let Some(arrive) = link.send(t_us, bytes.max(100)) {
                arrivals.push((
                    arrive,
                    FrameArrival {
                        send_us: t_us,
                        recv_us: arrive,
                        bytes,
                        is_key: false,
                    },
                ));
            }
            // Deliver everything that has arrived by now (the controller only ever sees arrivals).
            arrivals.sort_by_key(|(a, _)| *a);
            while arrivals.first().is_some_and(|(a, _)| *a <= t_us) {
                let (_, fa) = arrivals.remove(0);
                rc.on_frame(fa);
            }
            if let Some(new) = rc.poll() {
                // The decision reaches the sender one propagation delay later; model it as immediate
                // plus the link delay below.
                sender_rate = new;
            }
            trace.push((secs_now, rc.target_bps()));
            t_us += step_us;
        }
        (rc.target_bps(), trace, link.max_queue_ms)
    }

    fn at(trace: &[(f64, u32)], t: f64) -> u32 {
        trace
            .iter()
            .min_by(|a, b| (a.0 - t).abs().total_cmp(&(b.0 - t).abs()))
            .map_or(0, |p| p.1)
    }

    #[test]
    fn converges_near_a_steady_capacity_without_building_a_big_queue() {
        let (final_bps, trace, max_queue_ms) = simulate(40.0, 60.0, 0, |_| 10_000_000.0);
        println!("steady 10 Mbit/s: final {final_bps}, max queue {max_queue_ms:.0} ms");
        assert!(
            (6_500_000..=10_500_000).contains(&final_bps),
            "final target {final_bps}"
        );
        assert!(
            at(&trace, 20.0) > 5_000_000,
            "should have ramped up by t=20s: {}",
            at(&trace, 20.0)
        );
        assert!(
            max_queue_ms < 500.0,
            "queueing delay must stay bounded, saw {max_queue_ms:.0} ms"
        );
    }

    #[test]
    fn reacts_quickly_to_a_capacity_drop() {
        let (_final, trace, max_queue_ms) = simulate(30.0, 60.0, 0, |t| {
            if t < 15.0 { 20_000_000.0 } else { 4_000_000.0 }
        });
        let before = at(&trace, 14.0);
        let after = at(&trace, 17.0);
        println!(
            "drop 20→4 Mbit/s at t=15: target {before} → {after}, max queue {max_queue_ms:.0} ms"
        );
        assert!(before > 10_000_000, "had ramped up: {before}");
        assert!(
            after < 5_500_000,
            "must be below the new capacity within 2 s: {after}"
        );
        let reaction = trace
            .iter()
            .find(|(t, bps)| *t >= 15.0 && *bps < 5_500_000)
            .map(|(t, _)| (t - 15.0) * 1000.0);
        println!("reaction time to a 5x capacity drop: {reaction:?} ms");
        assert!(
            reaction.is_some_and(|r| r < 1000.0),
            "reaction {reaction:?} ms"
        );
        assert!(max_queue_ms < 1500.0, "queue delay {max_queue_ms:.0} ms");
    }

    #[test]
    fn grows_again_when_capacity_returns() {
        let (_f, trace, _q) = simulate(60.0, 60.0, 0, |t| {
            if (15.0..30.0).contains(&t) {
                3_000_000.0
            } else {
                20_000_000.0
            }
        });
        assert!(
            at(&trace, 28.0) < 4_000_000,
            "constrained phase: {}",
            at(&trace, 28.0)
        );
        assert!(
            at(&trace, 58.0) > 8_000_000,
            "recovered phase: {}",
            at(&trace, 58.0)
        );
    }

    #[test]
    fn jitter_alone_does_not_collapse_the_rate() {
        let (final_bps, _t, _q) = simulate(40.0, 60.0, 3_000, |_| 15_000_000.0);
        println!("steady 15 Mbit/s with ±3 ms jitter: final {final_bps}");
        assert!(
            final_bps > 5_000_000,
            "jitter must not look like congestion: {final_bps}"
        );
    }

    #[test]
    fn a_low_frame_rate_can_recover_bandwidth_without_waiting_minutes() {
        let mut rc = RateController::new(RateConfig::default());
        for i in 0..4u64 {
            rc.on_frame(FrameArrival {
                send_us: i * 1_000_000,
                recv_us: i * 1_000_000 + 20_000,
                bytes: 1_000,
                is_key: false,
            });
        }
        assert!(rc.target_bps() > 10_000_000, "{}", rc.target_bps());
    }

    #[test]
    fn retransmitted_old_streams_do_not_collapse_a_healthy_path() {
        let mut rc = RateController::new(RateConfig::default());
        for i in 1..1200u64 {
            let send_us = i * 16_667;
            rc.on_frame(FrameArrival {
                send_us,
                recv_us: send_us + 20_000,
                bytes: 8_000,
                is_key: false,
            });
            // Wi-Fi retransmission: an old per-frame stream finishes 200 ms late while
            // current frames keep arriving promptly. Include delayed keyframes as well.
            if i > 12 && i % 20 == 0 {
                let before = rc.target_bps();
                rc.on_frame(FrameArrival {
                    send_us: send_us - 200_004,
                    recv_us: send_us + 21_000,
                    bytes: 8_000,
                    is_key: i % 40 == 0,
                });
                assert_eq!(
                    rc.target_bps(),
                    before,
                    "an old stream must not reduce the current rate"
                );
            }
        }
        assert!(rc.target_bps() >= 4_000_000, "{}", rc.target_bps());
        assert_ne!(rc.usage(), Usage::Overuse);
    }

    #[test]
    fn heavy_reported_loss_reduces_the_rate() {
        let mut rc = RateController::new(RateConfig {
            start_bps: 20_000_000,
            ..Default::default()
        });
        rc.on_loss(0, 100, 0.0);
        assert_eq!(rc.target_bps(), 20_000_000);
        rc.on_loss(30, 100, 1000.0);
        assert!(rc.target_bps() < 18_000_000, "{}", rc.target_bps());
        assert!(rc.poll().is_some());
        assert!(
            rc.poll().is_none(),
            "unchanged targets are not re-announced"
        );
    }

    #[test]
    fn limits_are_respected() {
        let mut rc = RateController::new(RateConfig {
            min_bps: 500_000,
            max_bps: 3_000_000,
            start_bps: 1_000_000,
        });
        for i in 0..2000u64 {
            rc.on_frame(FrameArrival {
                send_us: i * 16_667,
                recv_us: i * 16_667 + 20_000,
                bytes: 4_000,
                is_key: false,
            });
        }
        assert!(rc.target_bps() <= 3_000_000);
        rc.on_loss(90, 100, 1e9);
        assert!(rc.target_bps() >= 500_000);
    }

    #[test]
    fn detector_flags_a_building_queue_and_a_draining_one() {
        let mut d = TrendDetector::new();
        let mut recv = 0u64;
        let mut verdicts = Vec::new();
        // Arrival gaps grow by 2 ms per frame: a queue building.
        for i in 0..60u64 {
            recv += 16_667 + i * 2_000;
            verdicts.push(d.update(FrameArrival {
                send_us: i * 16_667,
                recv_us: recv,
                bytes: 1000,
                is_key: false,
            }));
        }
        assert!(
            verdicts.contains(&Usage::Overuse),
            "growing delay must be flagged: {verdicts:?}"
        );
        // Gaps shrink: draining.
        let mut under = false;
        for i in 60..140u64 {
            recv += 16_667u64.saturating_sub((i - 60) * 400);
            under |= d.update(FrameArrival {
                send_us: i * 16_667,
                recv_us: recv,
                bytes: 1000,
                is_key: false,
            }) == Usage::Underuse;
        }
        assert!(under, "shrinking delay must be flagged as underuse");
    }
}
