//! Controller-side playback pipeline: datagrams in, PCM out, arbitrary callback sizes.

use bytes::Bytes;
use nexus_proto::wire::audio::{self, AudioCodec};

use crate::jitter::{JitterBuffer, Pull};
use crate::{AudioError, OpusDecoder, StreamConfig};

/// What the player did, for the HUD.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct PlayerStats {
    /// Frames decoded from received packets.
    pub decoded: u64,
    /// Lost frames rebuilt from the next packet's FEC.
    pub recovered: u64,
    /// Lost frames synthesised by packet-loss concealment.
    pub concealed: u64,
    /// Frames of silence played because nothing was available.
    pub silent: u64,
    /// Datagrams ignored (wrong shape, corrupt, undecodable).
    pub rejected: u64,
}

/// Feeds a playback device. Not thread-safe by itself: wrap it in a mutex shared between the
/// network task (`push`) and the device callback (`fill`).
#[derive(Debug)]
pub struct Player {
    cfg: StreamConfig,
    jitter: JitterBuffer,
    decoder: OpusDecoder,
    /// Decoded samples not yet handed to the device.
    carry: Vec<f32>,
    carry_pos: usize,
    scratch: Vec<f32>,
    stats: PlayerStats,
}

impl Player {
    /// A player for streams shaped like `cfg`.
    pub fn new(cfg: StreamConfig) -> Result<Self, AudioError> {
        Ok(Self {
            jitter: JitterBuffer::new(cfg.frame.micros()),
            decoder: OpusDecoder::new(&cfg)?,
            carry: vec![0.0; cfg.frame_len()],
            carry_pos: cfg.frame_len(),
            scratch: vec![0.0; cfg.frame_len()],
            stats: PlayerStats::default(),
            cfg,
        })
    }

    /// Stream shape.
    #[must_use]
    pub fn config(&self) -> StreamConfig {
        self.cfg
    }

    /// Counters so far.
    #[must_use]
    pub fn stats(&self) -> PlayerStats {
        self.stats
    }

    /// Jitter-buffer counters.
    #[must_use]
    pub fn jitter_stats(&self) -> crate::JitterStats {
        self.jitter.stats()
    }

    /// Accepts one received audio datagram. Anything that does not match the negotiated stream
    /// is counted and dropped; nothing here can panic on hostile input.
    pub fn push(&mut self, datagram: &[u8], now_us: u64) {
        let Ok((header, payload)) = audio::decode_packet(datagram) else {
            self.stats.rejected += 1;
            return;
        };
        if header.codec != AudioCodec::Opus
            || header.channels != self.cfg.channels
            || usize::from(header.frame_samples) != self.cfg.frame.samples()
            || payload.is_empty()
        {
            self.stats.rejected += 1;
            return;
        }
        self.jitter
            .push(&header, Bytes::copy_from_slice(payload), now_us);
    }

    /// Fills `out` (interleaved, `cfg.channels` wide) with the next samples to play.
    pub fn fill(&mut self, out: &mut [f32]) {
        let mut done = 0;
        while done < out.len() {
            if self.carry_pos >= self.carry.len() {
                self.next_frame();
            }
            let n = (out.len() - done).min(self.carry.len() - self.carry_pos);
            out[done..done + n].copy_from_slice(&self.carry[self.carry_pos..self.carry_pos + n]);
            self.carry_pos += n;
            done += n;
        }
    }

    fn next_frame(&mut self) {
        self.carry_pos = 0;
        let ok = match self.jitter.pull() {
            Pull::Packet(p) => {
                let r = self.decoder.decode(&p, &mut self.scratch);
                if r.is_ok() {
                    self.stats.decoded += 1;
                }
                r.is_ok()
            }
            Pull::Lost { next: Some(n) } => {
                let r = self.decoder.recover_from(&n, &mut self.scratch);
                if r.is_ok() {
                    self.stats.recovered += 1;
                }
                r.is_ok()
            }
            Pull::Lost { next: None } => {
                let r = self.decoder.conceal(&mut self.scratch);
                if r.is_ok() {
                    self.stats.concealed += 1;
                }
                r.is_ok()
            }
            Pull::Wait => {
                self.stats.silent += 1;
                self.scratch.fill(0.0);
                true
            }
        };
        if !ok {
            self.stats.rejected += 1;
            self.scratch.fill(0.0);
        }
        std::mem::swap(&mut self.carry, &mut self.scratch);
    }
}
