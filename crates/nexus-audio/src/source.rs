//! Where captured audio comes from.

use std::time::{Duration, Instant};

use crate::{AudioError, SAMPLE_RATE};

/// A blocking producer of fixed-size interleaved frames at 48 kHz.
pub trait AudioSource: Send {
    /// Short name for logs and the HUD.
    fn name(&self) -> &str;

    /// Fills `out` (exactly one frame) and returns when it is full. Real devices block until the
    /// hardware has produced that much audio; [`AudioError::Ended`] means no more will come.
    fn read(&mut self, out: &mut [f32]) -> Result<(), AudioError>;
}

/// A sine tone: the synthetic source for tests, benchmarks and `--tone` demos.
#[derive(Debug)]
pub struct SineSource {
    channels: usize,
    step: f32,
    phase: f32,
    amplitude: f32,
    /// Sleep so frames come out at real-time pace (otherwise as fast as asked).
    realtime: Option<(Instant, u64)>,
    frames_total: Option<u64>,
}

impl SineSource {
    /// A tone at `hz`, half scale.
    #[must_use]
    pub fn new(channels: u8, hz: f32) -> Self {
        Self {
            channels: usize::from(channels.max(1)),
            step: std::f32::consts::TAU * hz / SAMPLE_RATE as f32,
            phase: 0.0,
            amplitude: 0.5,
            realtime: None,
            frames_total: None,
        }
    }

    /// Produces audio at the pace of a real clock.
    #[must_use]
    pub fn realtime(mut self) -> Self {
        self.realtime = Some((Instant::now(), 0));
        self
    }

    /// Ends after `n` frames.
    #[must_use]
    pub fn limit_frames(mut self, n: u64) -> Self {
        self.frames_total = Some(n);
        self
    }
}

impl AudioSource for SineSource {
    fn name(&self) -> &str {
        "tone"
    }

    fn read(&mut self, out: &mut [f32]) -> Result<(), AudioError> {
        if let Some(left) = &mut self.frames_total {
            if *left == 0 {
                return Err(AudioError::Ended);
            }
            *left -= 1;
        }
        let per_channel = out.len() / self.channels;
        if let Some((start, emitted)) = &mut self.realtime {
            *emitted += per_channel as u64;
            let due = Duration::from_micros(*emitted * 1_000_000 / u64::from(SAMPLE_RATE));
            if let Some(wait) = due.checked_sub(start.elapsed()) {
                std::thread::sleep(wait);
            }
        }
        for frame in out.chunks_mut(self.channels) {
            let v = self.phase.sin() * self.amplitude;
            frame.fill(v);
            self.phase = (self.phase + self.step) % std::f32::consts::TAU;
        }
        Ok(())
    }
}

/// Plays back samples already in memory, then ends.
#[derive(Debug)]
pub struct PcmSource {
    samples: Vec<f32>,
    pos: usize,
}

impl PcmSource {
    /// Interleaved samples.
    #[must_use]
    pub fn new(samples: Vec<f32>) -> Self {
        Self { samples, pos: 0 }
    }
}

impl AudioSource for PcmSource {
    fn name(&self) -> &str {
        "pcm"
    }

    fn read(&mut self, out: &mut [f32]) -> Result<(), AudioError> {
        let end = self.pos + out.len();
        if end > self.samples.len() {
            return Err(AudioError::Ended);
        }
        out.copy_from_slice(&self.samples[self.pos..end]);
        self.pos = end;
        Ok(())
    }
}
