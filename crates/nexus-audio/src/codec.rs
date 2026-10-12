//! Opus encoder and decoder tuned for interactive system audio.

use opus::{Application, Bitrate, Channels};

use crate::{AudioError, SAMPLE_RATE, StreamConfig};

const fn channels(n: u8) -> Channels {
    if n == 1 {
        Channels::Mono
    } else {
        Channels::Stereo
    }
}

/// Largest Opus packet we produce (fits one datagram with the 12-byte audio header).
const MAX_PACKET: usize =
    nexus_proto::wire::audio::MAX_PACKET_LEN - nexus_proto::wire::audio::HEADER_LEN;

/// Encodes fixed-size frames of interleaved `f32` PCM.
pub struct OpusEncoder {
    enc: opus::Encoder,
    frame_len: usize,
    out: Vec<u8>,
}

impl std::fmt::Debug for OpusEncoder {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OpusEncoder")
            .field("frame_len", &self.frame_len)
            .finish_non_exhaustive()
    }
}

impl OpusEncoder {
    /// Creates an encoder with in-band FEC on and the low-delay application type.
    pub fn new(cfg: &StreamConfig) -> Result<Self, AudioError> {
        cfg.validate()?;
        let mut enc =
            opus::Encoder::new(SAMPLE_RATE, channels(cfg.channels), Application::LowDelay)?;
        enc.set_bitrate(Bitrate::Bits(
            i32::try_from(cfg.bitrate_bps).unwrap_or(96_000),
        ))?;
        // Constrained VBR keeps packets (and so the network load) predictable.
        enc.set_vbr(true)?;
        enc.set_vbr_constraint(true)?;
        enc.set_inband_fec(true)?;
        enc.set_packet_loss_perc(5)?;
        Ok(Self {
            enc,
            frame_len: cfg.frame_len(),
            out: vec![0; MAX_PACKET],
        })
    }

    /// Encodes exactly one frame (`frame_samples × channels` interleaved samples).
    pub fn encode(&mut self, pcm: &[f32]) -> Result<&[u8], AudioError> {
        if pcm.len() != self.frame_len {
            return Err(AudioError::Config(format!(
                "frame of {} samples, expected {}",
                pcm.len(),
                self.frame_len
            )));
        }
        let n = self.enc.encode_float(pcm, &mut self.out)?;
        Ok(&self.out[..n])
    }

    /// Changes the target bitrate on the fly (congestion response).
    pub fn set_bitrate(&mut self, bps: u32) -> Result<(), AudioError> {
        let bps = bps.clamp(6_000, 510_000);
        self.enc
            .set_bitrate(Bitrate::Bits(i32::try_from(bps).unwrap_or(96_000)))?;
        Ok(())
    }

    /// Tells the encoder how much loss to protect against (0..=100 %): more redundancy in FEC.
    pub fn set_expected_loss(&mut self, percent: u8) -> Result<(), AudioError> {
        self.enc.set_packet_loss_perc(i32::from(percent.min(100)))?;
        Ok(())
    }
}

/// Decodes packets, concealing or recovering lost ones.
pub struct OpusDecoder {
    dec: opus::Decoder,
    channels: usize,
    frame_samples: usize,
}

impl std::fmt::Debug for OpusDecoder {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OpusDecoder")
            .field("channels", &self.channels)
            .field("frame_samples", &self.frame_samples)
            .finish_non_exhaustive()
    }
}

impl OpusDecoder {
    /// Creates a decoder for streams shaped like `cfg`.
    pub fn new(cfg: &StreamConfig) -> Result<Self, AudioError> {
        cfg.validate()?;
        Ok(Self {
            dec: opus::Decoder::new(SAMPLE_RATE, channels(cfg.channels))?,
            channels: usize::from(cfg.channels),
            frame_samples: cfg.frame.samples(),
        })
    }

    /// Decodes one packet into `out` (one frame).
    pub fn decode(&mut self, packet: &[u8], out: &mut [f32]) -> Result<(), AudioError> {
        self.run(packet, out, false)
    }

    /// Recovers the frame *before* `next_packet` from the redundancy it carries.
    pub fn recover_from(&mut self, next_packet: &[u8], out: &mut [f32]) -> Result<(), AudioError> {
        self.run(next_packet, out, true)
    }

    /// Synthesises a replacement for a lost frame with no packet available.
    pub fn conceal(&mut self, out: &mut [f32]) -> Result<(), AudioError> {
        self.run(&[], out, false)
    }

    fn run(&mut self, packet: &[u8], out: &mut [f32], fec: bool) -> Result<(), AudioError> {
        let len = self.frame_samples * self.channels;
        let out = out.get_mut(..len).ok_or_else(|| {
            AudioError::Config(format!("output smaller than a {len}-sample frame"))
        })?;
        let n = self.dec.decode_float(packet, out, fec)?;
        if n != self.frame_samples {
            return Err(AudioError::Opus(format!(
                "decoded {n} samples per channel, expected {}",
                self.frame_samples
            )));
        }
        Ok(())
    }
}
