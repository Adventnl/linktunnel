//! The target-side capture → encode → send loop, on its own OS thread.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::thread::JoinHandle;

use bytes::Bytes;
use tracing::{debug, warn};

use crate::{AudioError, AudioSource, OpusEncoder, Packetizer, StreamConfig};

/// Live counters.
#[derive(Debug, Default)]
pub struct StreamStats {
    /// Packets handed to the sender.
    pub packets: AtomicU64,
    /// Bytes handed to the sender (datagram sizes).
    pub bytes: AtomicU64,
    /// Frames captured but not sent because the stream was muted.
    pub muted_frames: AtomicU64,
}

/// Controls a running stream; dropping it stops the thread.
#[derive(Debug)]
pub struct StreamHandle {
    stop: Arc<AtomicBool>,
    muted: Arc<AtomicBool>,
    bitrate: Arc<AtomicU32>,
    loss: Arc<AtomicU32>,
    stats: Arc<StreamStats>,
    thread: Option<JoinHandle<()>>,
}

impl StreamHandle {
    /// Counters.
    #[must_use]
    pub fn stats(&self) -> &Arc<StreamStats> {
        &self.stats
    }

    /// Stops sending audio but keeps draining the device (so unmuting is instant and the device
    /// buffer cannot grow stale). The receiver is told about the gap with a discontinuity flag.
    pub fn set_muted(&self, muted: bool) {
        self.muted.store(muted, Ordering::SeqCst);
    }

    /// Asks the encoder for a new bitrate before the next frame.
    pub fn set_bitrate(&self, bps: u32) {
        self.bitrate.store(bps, Ordering::SeqCst);
    }

    /// Expected packet loss in percent, from receiver reports (adds FEC redundancy).
    pub fn set_expected_loss(&self, percent: u8) {
        self.loss.store(u32::from(percent), Ordering::SeqCst);
    }

    /// True once the thread has exited (source ended, sender closed, or stopped).
    #[must_use]
    pub fn is_finished(&self) -> bool {
        self.thread.as_ref().is_none_or(JoinHandle::is_finished)
    }

    /// Stops and waits for the thread.
    pub fn stop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

impl Drop for StreamHandle {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        // Not joined here: a source blocked in the OS must not stall an async drop.
    }
}

/// Starts streaming `source` through `send`, which returns `false` when the session is gone.
pub fn spawn_stream(
    mut source: Box<dyn AudioSource>,
    cfg: StreamConfig,
    mut send: Box<dyn FnMut(Bytes) -> bool + Send>,
) -> Result<StreamHandle, AudioError> {
    let mut encoder = OpusEncoder::new(&cfg)?;
    let mut packetizer = Packetizer::new(&cfg);
    let stop = Arc::new(AtomicBool::new(false));
    let muted = Arc::new(AtomicBool::new(false));
    let bitrate = Arc::new(AtomicU32::new(cfg.bitrate_bps));
    let loss = Arc::new(AtomicU32::new(5));
    let stats = Arc::new(StreamStats::default());
    let thread = {
        let (stop, muted, bitrate, loss, stats) = (
            Arc::clone(&stop),
            Arc::clone(&muted),
            Arc::clone(&bitrate),
            Arc::clone(&loss),
            Arc::clone(&stats),
        );
        std::thread::Builder::new()
            .name("nexus-audio-send".into())
            .spawn(move || {
                let mut pcm = vec![0.0f32; cfg.frame_len()];
                let (mut cur_bitrate, mut cur_loss) = (cfg.bitrate_bps, 5u32);
                let mut muted_run: u64 = 0;
                debug!(source = source.name(), "audio stream started");
                while !stop.load(Ordering::SeqCst) {
                    match source.read(&mut pcm) {
                        Ok(()) => {}
                        Err(AudioError::Ended) => break,
                        Err(e) => {
                            warn!(error = %e, "audio source failed");
                            break;
                        }
                    }
                    if muted.load(Ordering::SeqCst) {
                        muted_run += 1;
                        stats.muted_frames.fetch_add(1, Ordering::Relaxed);
                        continue;
                    }
                    if muted_run > 0 {
                        // Account for the skipped time so the receiver's clock stays honest.
                        let samples = muted_run.saturating_mul(cfg.frame.samples() as u64);
                        packetizer.mark_discontinuity(u32::try_from(samples).unwrap_or(u32::MAX));
                        muted_run = 0;
                    }
                    let want = bitrate.load(Ordering::SeqCst);
                    if want != cur_bitrate {
                        cur_bitrate = want;
                        let _ = encoder.set_bitrate(want);
                    }
                    let l = loss.load(Ordering::SeqCst);
                    if l != cur_loss {
                        cur_loss = l;
                        let _ = encoder.set_expected_loss(u8::try_from(l).unwrap_or(100));
                    }
                    let Ok(frame) = encoder.encode(&pcm) else {
                        continue;
                    };
                    let Ok(datagram) = packetizer.packetize(frame) else {
                        continue;
                    };
                    let len = datagram.len() as u64;
                    if !send(datagram) {
                        break;
                    }
                    stats.packets.fetch_add(1, Ordering::Relaxed);
                    stats.bytes.fetch_add(len, Ordering::Relaxed);
                }
                debug!("audio stream ended");
            })
            .map_err(|e| AudioError::Device(format!("cannot start the audio thread: {e}")))?
    };
    Ok(StreamHandle {
        stop,
        muted,
        bitrate,
        loss,
        stats,
        thread: Some(thread),
    })
}
