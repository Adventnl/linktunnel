//! System-audio streaming for remote sessions.
//!
//! ```text
//!  target                                                        controller
//!  AudioSource ─▶ OpusEncoder ─▶ Packetizer ══ datagrams ══▶ Player { JitterBuffer ─▶ OpusDecoder } ─▶ device
//!  (loopback)     (10 ms, FEC)    (seq + sample clock)         (reorder, conceal loss)
//! ```
//!
//! * Audio rides unreliable QUIC datagrams: a late packet is worthless, so loss is *concealed*
//!   (Opus packet-loss concealment, or in-band FEC when the next packet has arrived) rather than
//!   retransmitted.
//! * The jitter buffer is adaptive: near zero on a quiet LAN, deeper when arrival times wobble.
//! * Everything here is independent of the network and of the OS; real devices live behind the
//!   `device` feature so the codec logic is testable anywhere.

#![forbid(unsafe_code)]

mod codec;
mod error;
mod jitter;
mod packet;
mod player;
mod source;
mod streamer;

#[cfg(feature = "device")]
pub mod device;

pub use codec::{OpusDecoder, OpusEncoder};
pub use error::AudioError;
pub use jitter::{JitterBuffer, JitterStats, Pull};
pub use packet::Packetizer;
pub use player::{Player, PlayerStats};
pub use source::{AudioSource, PcmSource, SineSource};
pub use streamer::{StreamHandle, StreamStats, spawn_stream};

/// The only sample rate on the wire; devices at other rates are resampled by the OS backend.
pub const SAMPLE_RATE: u32 = 48_000;

/// How much audio one packet carries. Smaller is lower latency and higher overhead.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FrameSize {
    /// 2.5 ms (only the CELT mode; best for music/games, highest packet rate).
    Ms2_5,
    /// 5 ms.
    Ms5,
    /// 10 ms: the default trade-off.
    Ms10,
    /// 20 ms: lowest overhead, still interactive.
    Ms20,
}

impl FrameSize {
    /// Samples per channel at 48 kHz.
    #[must_use]
    pub const fn samples(self) -> usize {
        match self {
            Self::Ms2_5 => 120,
            Self::Ms5 => 240,
            Self::Ms10 => 480,
            Self::Ms20 => 960,
        }
    }

    /// Duration in microseconds.
    #[must_use]
    pub const fn micros(self) -> u64 {
        match self {
            Self::Ms2_5 => 2_500,
            Self::Ms5 => 5_000,
            Self::Ms10 => 10_000,
            Self::Ms20 => 20_000,
        }
    }

    /// Parses a frame length in milliseconds (`2` stands for 2.5).
    #[must_use]
    pub const fn from_ms(ms: u32) -> Option<Self> {
        match ms {
            2 | 3 => Some(Self::Ms2_5),
            5 => Some(Self::Ms5),
            10 => Some(Self::Ms10),
            20 => Some(Self::Ms20),
            _ => None,
        }
    }

    /// The sample count as the 16-bit value carried in the packet header.
    #[must_use]
    pub const fn from_samples(samples: usize) -> Option<Self> {
        match samples {
            120 => Some(Self::Ms2_5),
            240 => Some(Self::Ms5),
            480 => Some(Self::Ms10),
            960 => Some(Self::Ms20),
            _ => None,
        }
    }
}

/// What a stream looks like.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StreamConfig {
    /// 1 or 2.
    pub channels: u8,
    /// Packet duration.
    pub frame: FrameSize,
    /// Target Opus bitrate.
    pub bitrate_bps: u32,
}

impl Default for StreamConfig {
    fn default() -> Self {
        Self {
            channels: 2,
            frame: FrameSize::Ms10,
            bitrate_bps: 96_000,
        }
    }
}

impl StreamConfig {
    /// Interleaved samples in one frame.
    #[must_use]
    pub const fn frame_len(&self) -> usize {
        self.frame.samples() * self.channels as usize
    }

    /// Rejects configurations the codec or wire format cannot carry.
    pub fn validate(&self) -> Result<(), AudioError> {
        if !(1..=2).contains(&self.channels) {
            return Err(AudioError::Config(format!(
                "{} channels (1 or 2 supported)",
                self.channels
            )));
        }
        if !(6_000..=510_000).contains(&self.bitrate_bps) {
            return Err(AudioError::Config(format!(
                "bitrate {} bps outside 6000..=510000",
                self.bitrate_bps
            )));
        }
        Ok(())
    }
}
