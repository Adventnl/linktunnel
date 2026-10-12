//! Turns encoded frames into numbered datagrams.

use bytes::{Bytes, BytesMut};
use nexus_proto::wire::audio::{self, AudioCodec, AudioHeader};

use crate::{AudioError, StreamConfig};

/// Stamps consecutive packets with a sequence number and a 48 kHz sample-clock timestamp.
#[derive(Debug)]
pub struct Packetizer {
    channels: u8,
    frame_samples: u16,
    seq: u16,
    timestamp: u32,
    discontinuity: bool,
}

impl Packetizer {
    /// Starts at sequence 0.
    #[must_use]
    pub fn new(cfg: &StreamConfig) -> Self {
        Self {
            channels: cfg.channels,
            // 960 at most, so the cast cannot truncate.
            frame_samples: u16::try_from(cfg.frame.samples()).unwrap_or(480),
            seq: 0,
            timestamp: 0,
            discontinuity: false,
        }
    }

    /// Marks that audio was dropped before the next packet (the receiver should re-buffer).
    pub fn mark_discontinuity(&mut self, skipped_samples: u32) {
        self.timestamp = self.timestamp.wrapping_add(skipped_samples);
        self.discontinuity = true;
    }

    /// Wraps one encoded Opus frame.
    pub fn packetize(&mut self, opus_frame: &[u8]) -> Result<Bytes, AudioError> {
        let header = AudioHeader {
            codec: AudioCodec::Opus,
            channels: self.channels,
            flags: if self.discontinuity {
                audio::FLAG_DISCONTINUITY
            } else {
                0
            },
            seq: self.seq,
            frame_samples: self.frame_samples,
            timestamp: self.timestamp,
        };
        let mut out = BytesMut::with_capacity(audio::HEADER_LEN + opus_frame.len());
        audio::encode_packet(&header, opus_frame, &mut out)?;
        self.discontinuity = false;
        self.seq = self.seq.wrapping_add(1);
        self.timestamp = self.timestamp.wrapping_add(u32::from(self.frame_samples));
        Ok(out.freeze())
    }
}
