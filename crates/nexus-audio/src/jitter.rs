//! Receive-side reordering and loss detection for audio packets.
//!
//! The buffer is *pull-driven*: the playback device asks for one frame per period and gets a
//! packet, a "this one is lost" verdict (possibly with the next packet so FEC can recover it), or
//! "nothing yet". Its depth follows measured arrival jitter, so a quiet LAN runs at one frame of
//! delay and a wobbly Wi-Fi link grows only as far as it must.

use std::collections::BTreeMap;

use bytes::Bytes;
use nexus_proto::wire::audio::{AudioHeader, FLAG_DISCONTINUITY};

/// What the playback side should do for its next frame.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Pull {
    /// Decode this packet.
    Packet(Bytes),
    /// The expected packet never came. If the packet after it is already here, `next` lets the
    /// decoder recover this frame from its in-band FEC; otherwise conceal.
    Lost {
        /// The following packet, if buffered.
        next: Option<Bytes>,
    },
    /// Still filling, or the sender is quiet: play silence, nothing is wrong.
    Wait,
}

/// Counters for the HUD and the tests.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct JitterStats {
    /// Packets accepted.
    pub received: u64,
    /// Frames declared lost.
    pub lost: u64,
    /// Packets that arrived after their slot had played (or duplicates).
    pub late: u64,
    /// Times playback restarted after running dry.
    pub underruns: u64,
    /// Current target depth in frames.
    pub target_frames: u32,
}

const MIN_TARGET: u32 = 1;
const MAX_TARGET: u32 = 16;
/// Empty pulls in a row before the buffer concludes the stream stopped and re-buffers.
const IDLE_PULLS: u32 = 25;

/// See the module docs.
#[derive(Debug)]
pub struct JitterBuffer {
    frame_us: u64,
    queue: BTreeMap<u64, Bytes>,
    /// Extended sequence number played next (`None` until playback starts).
    next: Option<u64>,
    /// Highest extended sequence number seen.
    highest: Option<u64>,
    playing: bool,
    idle: u32,
    /// RFC 3550 interarrival jitter, microseconds.
    jitter_us: f64,
    last_transit: Option<i64>,
    target: u32,
    stats: JitterStats,
}

impl JitterBuffer {
    /// `frame_us` is the packet duration.
    #[must_use]
    pub fn new(frame_us: u64) -> Self {
        Self {
            frame_us: frame_us.max(1),
            queue: BTreeMap::new(),
            next: None,
            highest: None,
            playing: false,
            idle: 0,
            jitter_us: 0.0,
            last_transit: None,
            target: MIN_TARGET,
            stats: JitterStats {
                target_frames: MIN_TARGET,
                ..JitterStats::default()
            },
        }
    }

    /// Counters so far.
    #[must_use]
    pub fn stats(&self) -> JitterStats {
        JitterStats {
            target_frames: self.target,
            ..self.stats
        }
    }

    /// Frames waiting to be played.
    #[must_use]
    pub fn buffered(&self) -> usize {
        self.queue.len()
    }

    /// Forgets everything and re-buffers (the sender flagged a discontinuity).
    fn reset(&mut self) {
        self.queue.clear();
        self.next = None;
        self.highest = None;
        self.playing = false;
        self.idle = 0;
        // The sender's clock baseline changed; the next arrival says nothing about jitter.
        self.last_transit = None;
    }

    /// Extends a 16-bit sequence number to 64 bits relative to the highest seen.
    fn extend(&self, seq: u16) -> u64 {
        let Some(hi) = self.highest else {
            return 1 << 32 | u64::from(seq);
        };
        let hi_lo = (hi & 0xFFFF) as u16;
        let delta = i64::from(seq.wrapping_sub(hi_lo) as i16);
        // `hi` starts at 2^32, so this cannot underflow in practice; saturate anyway.
        hi.saturating_add_signed(delta)
    }

    /// Offers a received packet. `arrival_us` is any monotonic clock in microseconds.
    pub fn push(&mut self, header: &AudioHeader, payload: Bytes, arrival_us: u64) {
        if header.flags & FLAG_DISCONTINUITY != 0 {
            self.reset();
        }
        // Interarrival jitter (RFC 3550 §A.8), with the 48 kHz sample clock converted to µs.
        let sent_us = i64::from(header.timestamp) * 1_000_000 / 48_000;
        let arrival = i64::try_from(arrival_us).unwrap_or(i64::MAX);
        let transit = arrival.wrapping_sub(sent_us);
        if let Some(prev) = self.last_transit {
            // Timestamp wrap (every ~24 h) would show up as a huge step; ignore such samples.
            let d = (transit.wrapping_sub(prev)).unsigned_abs() as f64;
            if d < 5_000_000.0 {
                self.jitter_us += (d - self.jitter_us) / 16.0;
            }
        }
        self.last_transit = Some(transit);
        self.retarget();
        let ext = self.extend(header.seq);
        if let Some(next) = self.next
            && (ext < next || self.queue.contains_key(&ext))
        {
            self.stats.late += 1;
            return;
        }

        self.highest = Some(self.highest.map_or(ext, |h| h.max(ext)));
        if self.next.is_none() {
            self.next = Some(ext);
        } else if let Some(n) = self.next {
            // Before playback starts, a reordered earlier packet moves the start back.
            if !self.playing && ext < n {
                self.next = Some(ext);
            }
        }
        self.queue.insert(ext, payload);
        self.stats.received += 1;
    }

    fn retarget(&mut self) {
        // Two standard deviations' worth of headroom, in frames, plus the base frame.
        let frames = (2.0 * self.jitter_us / self.frame_us as f64).ceil();
        // Clamped first, so the conversion is exact.
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let frames = frames.clamp(0.0, f64::from(MAX_TARGET)) as u32;
        let want = frames
            .saturating_add(MIN_TARGET)
            .clamp(MIN_TARGET, MAX_TARGET);
        // Grow immediately, shrink slowly (one frame per adjustment) to avoid flapping.
        self.target = if want >= self.target {
            want
        } else {
            self.target - 1
        };
    }

    /// Supplies what to play for the next frame slot.
    pub fn pull(&mut self) -> Pull {
        let Some(next) = self.next else {
            return Pull::Wait;
        };
        if !self.playing {
            let span = self.highest.map_or(0, |h| h - next + 1);
            if span >= u64::from(self.target) && self.queue.contains_key(&next) {
                self.playing = true;
            } else if span >= u64::from(self.target) + 2 {
                // The first packet never showed up: start from whatever we have.
                self.playing = true;
            } else {
                return Pull::Wait;
            }
        }
        if let Some(p) = self.queue.remove(&next) {
            self.next = Some(next + 1);
            self.idle = 0;
            return Pull::Packet(p);
        }
        let ahead = self.highest.map_or(0, |h| h.saturating_sub(next));
        if ahead >= u64::from(self.target) {
            // Later packets are here and the gap is old enough: give up on `next`.
            self.stats.lost += 1;
            self.next = Some(next + 1);
            self.idle = 0;
            return Pull::Lost {
                next: self.queue.get(&(next + 1)).cloned(),
            };
        }
        if self.queue.is_empty() {
            self.idle += 1;
            if self.idle >= IDLE_PULLS {
                // The sender went quiet (muted / source stopped): start over when it resumes.
                self.stats.underruns += 1;
                self.reset();
            }
        }
        Pull::Wait
    }
}

#[cfg(test)]
#[allow(clippy::cast_possible_truncation)]
mod tests {
    use nexus_proto::wire::audio::AudioCodec;

    use super::*;

    const FRAME: u64 = 10_000;

    fn hdr(seq: u16) -> AudioHeader {
        AudioHeader {
            codec: AudioCodec::Opus,
            channels: 2,
            flags: 0,
            seq,
            frame_samples: 480,
            timestamp: u32::from(seq).wrapping_mul(480),
        }
    }

    fn pkt(n: u8) -> Bytes {
        Bytes::from(vec![n])
    }

    fn push(j: &mut JitterBuffer, seq: u16, at_ms: u64) {
        j.push(&hdr(seq), pkt(seq as u8), at_ms * 1000);
    }

    #[test]
    fn in_order_stream_plays_every_packet_once() {
        let mut j = JitterBuffer::new(FRAME);
        for s in 0..20u16 {
            push(&mut j, s, u64::from(s) * 10);
        }
        let mut got = Vec::new();
        while let Pull::Packet(p) = j.pull() {
            got.push(p[0]);
        }
        assert_eq!(got, (0..20u8).collect::<Vec<_>>());
        assert_eq!(j.stats().lost, 0);
    }

    #[test]
    fn reordered_packets_are_put_back_in_order() {
        let mut j = JitterBuffer::new(FRAME);
        for (i, s) in [0u16, 2, 1, 3, 5, 4, 6].iter().enumerate() {
            push(&mut j, *s, i as u64 * 10);
        }
        let mut got = Vec::new();
        while let Pull::Packet(p) = j.pull() {
            got.push(p[0]);
        }
        assert_eq!(got, vec![0, 1, 2, 3, 4, 5, 6]);
    }

    #[test]
    fn a_missing_packet_is_reported_lost_with_the_next_for_fec() {
        let mut j = JitterBuffer::new(FRAME);
        for s in [0u16, 1, 3, 4, 5, 6] {
            push(&mut j, s, u64::from(s) * 10);
        }
        let mut out = Vec::new();
        for _ in 0..7 {
            out.push(j.pull());
        }
        assert_eq!(out[0], Pull::Packet(pkt(0)));
        assert_eq!(out[1], Pull::Packet(pkt(1)));
        assert_eq!(out[2], Pull::Lost { next: Some(pkt(3)) });
        assert_eq!(out[3], Pull::Packet(pkt(3)));
        assert_eq!(j.stats().lost, 1);
    }

    #[test]
    fn a_packet_arriving_after_its_slot_is_dropped_not_replayed() {
        let mut j = JitterBuffer::new(FRAME);
        for s in [0u16, 1, 3, 4, 5] {
            push(&mut j, s, u64::from(s) * 10);
        }
        while !matches!(j.pull(), Pull::Packet(ref p) if p[0] == 3) {}
        push(&mut j, 2, 60);
        assert_eq!(j.stats().late, 1);
        assert!(!matches!(j.pull(), Pull::Packet(ref p) if p[0] == 2));
    }

    #[test]
    fn sequence_wraparound_is_seamless() {
        let mut j = JitterBuffer::new(FRAME);
        let start = u16::MAX - 3;
        for i in 0..10u16 {
            let s = start.wrapping_add(i);
            j.push(&hdr(s), pkt(i as u8), u64::from(i) * 10_000);
        }
        let mut got = Vec::new();
        while let Pull::Packet(p) = j.pull() {
            got.push(p[0]);
        }
        assert_eq!(got, (0..10u8).collect::<Vec<_>>());
    }

    #[test]
    fn jitter_deepens_the_buffer_and_calm_lets_it_shrink() {
        let mut j = JitterBuffer::new(FRAME);
        // Arrival times wobble ±30 ms around the nominal schedule.
        for s in 0..200u16 {
            let wobble = if s % 2 == 0 { 0 } else { 30 };
            push(&mut j, s, u64::from(s) * 10 + wobble);
        }
        let deep = j.stats().target_frames;
        assert!(deep >= 3, "target stayed at {deep}");
        for s in 200..1200u16 {
            push(&mut j, s, u64::from(s) * 10);
        }
        assert!(j.stats().target_frames < deep);
        assert!(j.stats().target_frames <= MAX_TARGET);
    }

    #[test]
    fn discontinuity_flag_restarts_buffering() {
        let mut j = JitterBuffer::new(FRAME);
        for s in 0..5u16 {
            push(&mut j, s, u64::from(s) * 10);
        }
        let mut h = hdr(500);
        h.flags = FLAG_DISCONTINUITY;
        j.push(&h, pkt(9), 1_000_000);
        assert_eq!(j.buffered(), 1);
        assert_eq!(j.pull(), Pull::Packet(pkt(9)));
    }

    #[test]
    fn a_silent_sender_is_not_treated_as_loss_and_playback_restarts() {
        let mut j = JitterBuffer::new(FRAME);
        for s in 0..3u16 {
            push(&mut j, s, u64::from(s) * 10);
        }
        for _ in 0..3 {
            assert!(matches!(j.pull(), Pull::Packet(_)));
        }
        for _ in 0..40 {
            assert_eq!(j.pull(), Pull::Wait);
        }
        assert_eq!(j.stats().lost, 0);
        assert_eq!(j.stats().underruns, 1);
        // The sender resumes with an unrelated sequence number.
        for s in 900..903u16 {
            push(&mut j, s, 5_000 + u64::from(s - 900) * 10);
        }
        assert_eq!(j.pull(), Pull::Packet(pkt((900u16 & 0xFF) as u8)));
    }

    proptest::proptest! {
        #[test]
        fn never_panics_and_never_replays(order in proptest::collection::vec(0u16..40, 0..120)) {
            let mut j = JitterBuffer::new(FRAME);
            let mut played = std::collections::HashSet::new();
            for (i, s) in order.iter().enumerate() {
                push(&mut j, *s, i as u64 * 7);
                if i % 3 == 0
                    && let Pull::Packet(p) = j.pull()
                {
                    proptest::prop_assert!(played.insert(p[0]), "replayed {}", p[0]);
                }
            }
        }
    }
}
