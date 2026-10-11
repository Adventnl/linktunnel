//! Puts received video frames back in order and decides when a gap means loss.
//!
//! Every frame travels on its own QUIC stream, so frames can complete out of order. The decoder
//! needs them in order because each delta frame predicts from the previous one. The rules:
//!
//! * a **keyframe** is always deliverable immediately (it needs no history) and discards
//!   everything older;
//! * a **delta** is deliverable when it is the next expected sequence number; later ones wait,
//!   but only for [`Reorder::max_wait`]: after that the gap is declared lost, the receiver asks
//!   for a recovery frame, and deltas are dropped until one arrives;
//! * until the first keyframe, deltas are dropped.
//!
//! Pure logic with an injected clock, so every rule is unit tested deterministically.

use std::collections::BTreeMap;
use std::time::{Duration, Instant};

use nexus_proto::serial::{distance_u32, newer_u32};

/// Anything with a sequence number and a "needs no history" flag.
pub trait Sequenced {
    /// Frame sequence number.
    fn seq(&self) -> u32;
    /// True for frames that can be decoded with no prior frame.
    fn is_key(&self) -> bool;
}

/// What the caller should do next.
#[derive(Debug, PartialEq, Eq)]
pub enum Action<T> {
    /// Hand this frame to the decoder.
    Deliver(T),
    /// Tell the sender to produce a recovery frame.
    RequestKeyframe {
        /// Highest sequence number delivered so far (0 if none).
        last_good: u32,
    },
}

/// Counters for diagnostics.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct ReorderStats {
    /// Frames handed to the decoder.
    pub delivered: u64,
    /// Frames dropped because they were late, duplicate, or arrived while desynchronised.
    pub dropped: u64,
    /// Gaps declared lost.
    pub losses: u64,
}

/// The reordering buffer.
#[derive(Debug)]
pub struct Reorder<T: Sequenced> {
    next: u32,
    last_delivered: u32,
    synced: bool,
    pending: BTreeMap<u32, T>,
    gap_since: Option<Instant>,
    last_request: Option<Instant>,
    /// How long a gap is tolerated before it counts as loss.
    pub max_wait: Duration,
    /// Most frames held while waiting for a gap to fill.
    pub max_pending: usize,
    /// Minimum spacing between keyframe requests.
    pub request_interval: Duration,
    stats: ReorderStats,
}

impl<T: Sequenced> Reorder<T> {
    /// Creates an unsynchronised buffer (waits for a keyframe).
    #[must_use]
    pub fn new(max_wait: Duration) -> Self {
        Self {
            next: 0,
            last_delivered: 0,
            synced: false,
            pending: BTreeMap::new(),
            gap_since: None,
            last_request: None,
            max_wait,
            max_pending: 32,
            request_interval: Duration::from_millis(100),
            stats: ReorderStats::default(),
        }
    }

    /// Counters so far.
    #[must_use]
    pub fn stats(&self) -> ReorderStats {
        self.stats
    }

    /// True once a keyframe has been delivered and no loss is outstanding.
    #[must_use]
    pub fn is_synced(&self) -> bool {
        self.synced
    }

    /// Declares the stream broken from outside (e.g. the decoder failed): drop everything until
    /// the next keyframe and ask for one.
    pub fn desync(&mut self, now: Instant) -> Vec<Action<T>> {
        self.synced = false;
        self.pending.clear();
        self.gap_since = None;
        self.request(now).into_iter().collect()
    }

    /// Feeds a received frame.
    pub fn push(&mut self, frame: T, now: Instant) -> Vec<Action<T>> {
        let mut out = Vec::new();
        let seq = frame.seq();
        if frame.is_key() {
            if self.synced && !newer_u32(seq, self.last_delivered) {
                self.stats.dropped += 1; // a stale keyframe
                return out;
            }
            // Everything buffered is older than this keyframe or unusable without it.
            self.stats.dropped += self.pending.len() as u64;
            self.pending.clear();
            self.gap_since = None;
            self.deliver(frame, &mut out);
            self.synced = true;
            return out;
        }
        if !self.synced {
            self.stats.dropped += 1;
            out.extend(self.request(now));
            return out;
        }
        if seq == self.next {
            self.deliver(frame, &mut out);
            while let Some(f) = self.pending.remove(&self.next) {
                self.deliver(f, &mut out);
            }
            if self.pending.is_empty() {
                self.gap_since = None;
            }
        } else if newer_u32(seq, self.next) {
            self.pending.insert(seq, frame);
            self.gap_since.get_or_insert(now);
            if self.pending.len() > self.max_pending {
                out.extend(self.declare_loss(now));
            }
        } else {
            self.stats.dropped += 1; // duplicate or already skipped
        }
        out
    }

    /// Call periodically (every few milliseconds) so gaps time out.
    pub fn poll(&mut self, now: Instant) -> Vec<Action<T>> {
        match self.gap_since {
            Some(since) if now.duration_since(since) >= self.max_wait => self.declare_loss(now),
            _ => Vec::new(),
        }
    }

    /// When the next timeout can fire, for sleeping precisely.
    #[must_use]
    pub fn next_deadline(&self) -> Option<Instant> {
        self.gap_since.map(|s| s + self.max_wait)
    }

    fn deliver(&mut self, frame: T, out: &mut Vec<Action<T>>) {
        self.next = frame.seq().wrapping_add(1);
        self.last_delivered = frame.seq();
        self.stats.delivered += 1;
        out.push(Action::Deliver(frame));
    }

    fn declare_loss(&mut self, now: Instant) -> Vec<Action<T>> {
        self.stats.losses += 1;
        self.stats.dropped += self.pending.len() as u64;
        self.pending.clear();
        self.gap_since = None;
        self.synced = false;
        self.request(now).into_iter().collect()
    }

    fn request(&mut self, now: Instant) -> Option<Action<T>> {
        if self
            .last_request
            .is_some_and(|t| now.duration_since(t) < self.request_interval)
        {
            return None;
        }
        self.last_request = Some(now);
        Some(Action::RequestKeyframe {
            last_good: self.last_delivered,
        })
    }

    /// How far ahead of the next expected frame the newest buffered frame is.
    #[must_use]
    pub fn gap(&self) -> u32 {
        self.pending
            .keys()
            .next_back()
            .map_or(0, |last| distance_u32(self.next, *last))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Debug, PartialEq, Eq, Clone)]
    struct F(u32, bool);
    impl Sequenced for F {
        fn seq(&self) -> u32 {
            self.0
        }
        fn is_key(&self) -> bool {
            self.1
        }
    }

    fn delivered(actions: &[Action<F>]) -> Vec<u32> {
        actions
            .iter()
            .filter_map(|a| {
                if let Action::Deliver(f) = a {
                    Some(f.0)
                } else {
                    None
                }
            })
            .collect()
    }

    fn requests(actions: &[Action<F>]) -> usize {
        actions
            .iter()
            .filter(|a| matches!(a, Action::RequestKeyframe { .. }))
            .count()
    }

    fn rb() -> (Reorder<F>, Instant) {
        (Reorder::new(Duration::from_millis(50)), Instant::now())
    }

    #[test]
    fn in_order_frames_pass_straight_through() {
        let (mut r, t) = rb();
        assert_eq!(delivered(&r.push(F(1, true), t)), vec![1]);
        for n in 2..10 {
            assert_eq!(delivered(&r.push(F(n, false), t)), vec![n]);
        }
        assert_eq!(
            r.stats(),
            ReorderStats {
                delivered: 9,
                dropped: 0,
                losses: 0
            }
        );
    }

    #[test]
    fn deltas_before_the_first_keyframe_are_dropped_and_a_keyframe_requested_once() {
        let (mut r, t) = rb();
        let a = r.push(F(5, false), t);
        assert!(delivered(&a).is_empty());
        assert_eq!(requests(&a), 1);
        let b = r.push(F(6, false), t + Duration::from_millis(10));
        assert_eq!(requests(&b), 0, "rate limited");
        assert_eq!(
            delivered(&r.push(F(7, true), t + Duration::from_millis(20))),
            vec![7]
        );
        assert_eq!(
            delivered(&r.push(F(8, false), t + Duration::from_millis(30))),
            vec![8]
        );
    }

    #[test]
    fn out_of_order_frames_are_reassembled() {
        let (mut r, t) = rb();
        r.push(F(1, true), t);
        assert!(delivered(&r.push(F(3, false), t)).is_empty());
        assert!(delivered(&r.push(F(4, false), t)).is_empty());
        assert_eq!(
            r.gap(),
            2,
            "newest buffered frame is 2 ahead of the one we are waiting for"
        );
        assert_eq!(delivered(&r.push(F(2, false), t)), vec![2, 3, 4]);
        assert!(
            r.poll(t + Duration::from_secs(5)).is_empty(),
            "no gap left, nothing to time out"
        );
    }

    #[test]
    fn a_gap_that_never_fills_becomes_a_loss_and_a_keyframe_request() {
        let (mut r, t) = rb();
        r.push(F(1, true), t);
        r.push(F(3, false), t);
        assert!(r.poll(t + Duration::from_millis(49)).is_empty());
        let a = r.poll(t + Duration::from_millis(50));
        assert_eq!(a, vec![Action::RequestKeyframe { last_good: 1 }]);
        assert!(!r.is_synced());
        // Deltas are useless now; a keyframe resynchronises.
        assert!(delivered(&r.push(F(4, false), t + Duration::from_millis(60))).is_empty());
        assert_eq!(
            delivered(&r.push(F(9, true), t + Duration::from_millis(70))),
            vec![9]
        );
        assert!(r.is_synced());
        assert_eq!(
            delivered(&r.push(F(10, false), t + Duration::from_millis(80))),
            vec![10]
        );
        assert_eq!(r.stats().losses, 1);
    }

    #[test]
    fn a_keyframe_overtakes_waiting_frames() {
        let (mut r, t) = rb();
        r.push(F(1, true), t);
        r.push(F(3, false), t);
        r.push(F(4, false), t);
        assert_eq!(delivered(&r.push(F(6, true), t)), vec![6]);
        assert!(r.poll(t + Duration::from_secs(1)).is_empty());
        assert_eq!(delivered(&r.push(F(7, false), t)), vec![7]);
    }

    #[test]
    fn late_duplicates_and_stale_keyframes_are_ignored() {
        let (mut r, t) = rb();
        r.push(F(1, true), t);
        r.push(F(2, false), t);
        r.push(F(3, false), t);
        assert!(r.push(F(2, false), t).is_empty(), "duplicate");
        assert!(
            r.push(F(1, true), t).is_empty(),
            "old keyframe must not rewind the decoder"
        );
        assert_eq!(r.stats().dropped, 2);
    }

    #[test]
    fn sequence_numbers_wrap() {
        let (mut r, t) = rb();
        r.push(F(u32::MAX - 1, true), t);
        assert_eq!(delivered(&r.push(F(u32::MAX, false), t)), vec![u32::MAX]);
        assert!(
            delivered(&r.push(F(1, false), t)).is_empty(),
            "1 is ahead of 0, so it waits"
        );
        assert_eq!(delivered(&r.push(F(0, false), t)), vec![0, 1]);
    }

    #[test]
    fn too_many_waiting_frames_is_a_loss_without_waiting_for_the_timer() {
        let (mut r, t) = rb();
        r.max_pending = 3;
        r.push(F(1, true), t);
        let mut req = 0;
        for n in 3..8 {
            req += requests(&r.push(F(n, false), t));
        }
        assert_eq!(req, 1);
        assert!(!r.is_synced());
    }

    #[test]
    fn external_desync_requests_recovery() {
        let (mut r, t) = rb();
        r.push(F(1, true), t);
        r.push(F(2, false), t);
        let a = r.desync(t + Duration::from_millis(500));
        assert_eq!(a, vec![Action::RequestKeyframe { last_good: 2 }]);
        assert!(delivered(&r.push(F(3, false), t + Duration::from_millis(501))).is_empty());
    }

    #[test]
    fn next_deadline_tracks_the_oldest_gap() {
        let (mut r, t) = rb();
        assert!(r.next_deadline().is_none());
        r.push(F(1, true), t);
        r.push(F(3, false), t + Duration::from_millis(5));
        assert_eq!(r.next_deadline(), Some(t + Duration::from_millis(55)));
    }
}
