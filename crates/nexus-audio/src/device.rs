//! OS audio devices through cpal: system-audio capture (loopback) and playback.
//!
//! cpal streams are not `Send` on every platform, so each lives on its own thread that owns it
//! until told to stop; the rest of the program talks to it through a small shared ring.
//!
//! Platform notes (what "capture what the machine is playing" means):
//! * **Windows** — WASAPI loopback: an input stream built on the default *output* device.
//! * **macOS** — CoreAudio loopback through a private aggregate device (macOS 14.6+; needs the
//!   system-audio recording permission).
//! * **Linux** — the PulseAudio/PipeWire "monitor" of an output. Through ALSA this is chosen
//!   with `PULSE_SOURCE=<sink>.monitor` or by naming a device that contains "monitor".

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Condvar, Mutex};
use std::thread::JoinHandle;
use std::time::Duration;

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{SampleFormat, StreamConfig as DeviceStreamConfig};
use tracing::{debug, warn};

use crate::{AudioError, AudioSource, Player, SAMPLE_RATE, StreamConfig};

/// Never queue more than this much capture audio; older audio is dropped (a stalled consumer
/// must not turn into seconds of lag).
const MAX_QUEUED_MS: usize = 200;

fn dev_err(e: impl std::fmt::Display) -> AudioError {
    AudioError::Device(e.to_string())
}

/// Names of the devices the OS offers, for `nexus doctor` and the settings UI.
pub fn list_devices() -> Result<Vec<DeviceInfo>, AudioError> {
    let host = cpal::default_host();
    let default_in = host.default_input_device().map(|d| d.to_string());
    let default_out = host.default_output_device().map(|d| d.to_string());
    let mut out = Vec::new();
    for d in host.devices().map_err(dev_err)? {
        let name = d.to_string();
        out.push(DeviceInfo {
            input: d.supports_input(),
            output: d.supports_output(),
            default_input: default_in.as_deref() == Some(name.as_str()),
            default_output: default_out.as_deref() == Some(name.as_str()),
            name,
        });
    }
    Ok(out)
}

/// One audio device as the OS reports it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeviceInfo {
    /// Display name (also what `--audio-device` matches against).
    pub name: String,
    /// Can record.
    pub input: bool,
    /// Can play.
    pub output: bool,
    /// Is the default recording device.
    pub default_input: bool,
    /// Is the default playback device.
    pub default_output: bool,
}

/// A sample queue shared between a device callback and a consumer.
#[derive(Debug, Default)]
struct Ring {
    q: Mutex<VecDeque<f32>>,
    ready: Condvar,
}

/// Converts interleaved device samples (any channel count) to `want` channels.
fn remix(input: &[f32], have: usize, want: usize, out: &mut VecDeque<f32>) {
    if have == want {
        out.extend(input.iter().copied());
        return;
    }
    for frame in input.chunks_exact(have) {
        match want {
            1 => out.push_back(frame.iter().sum::<f32>() / have as f32),
            _ => {
                let l = frame[0];
                let r = if have > 1 { frame[1] } else { l };
                out.push_back(l);
                out.push_back(r);
            }
        }
    }
}

/// Picks a 48 kHz configuration with a sample format we can convert (f32, i16).
fn pick_config(
    device: &cpal::Device,
    want_channels: u8,
    input: bool,
) -> Result<(DeviceStreamConfig, SampleFormat), AudioError> {
    let mut best: Option<(u8, DeviceStreamConfig, SampleFormat)> = None;
    let ranges: Vec<_> = if input {
        device.supported_input_configs().map_err(dev_err)?.collect()
    } else {
        device
            .supported_output_configs()
            .map_err(dev_err)?
            .collect()
    };
    for r in ranges {
        if !r.contains_rate(SAMPLE_RATE)
            || !matches!(r.sample_format(), SampleFormat::F32 | SampleFormat::I16)
        {
            continue;
        }
        let Some(sc) = r.try_with_sample_rate(SAMPLE_RATE) else {
            continue;
        };
        // Prefer: exact channel count, then stereo, then anything; f32 over i16.
        let mut score = if r.channels() == u16::from(want_channels) {
            4
        } else if r.channels() == 2 {
            2
        } else {
            1
        };
        if r.sample_format() == SampleFormat::F32 {
            score += 1;
        }
        if best.as_ref().is_none_or(|(s, ..)| score > *s) {
            best = Some((score, sc.config(), sc.sample_format()));
        }
    }
    best.map(|(_, c, f)| (c, f)).ok_or_else(|| {
        AudioError::Device(
            "the device offers no 48 kHz f32/i16 configuration (resampling is not implemented)"
                .into(),
        )
    })
}

fn pick_input_device(
    host: &cpal::Host,
    name: Option<&str>,
    loopback: bool,
) -> Result<cpal::Device, AudioError> {
    if let Some(wanted) = name {
        let wanted = wanted.to_lowercase();
        return host
            .devices()
            .map_err(dev_err)?
            .find(|d| d.to_string().to_lowercase().contains(&wanted))
            .ok_or_else(|| AudioError::Device(format!("no audio device matching \"{wanted}\"")));
    }
    if loopback {
        if cfg!(any(windows, target_os = "macos")) {
            // The OS records an output device as loopback when an input stream is built on it.
            return host
                .default_output_device()
                .ok_or_else(|| AudioError::Device("no default output device to capture".into()));
        }
        if let Some(m) = host
            .input_devices()
            .map_err(dev_err)?
            .find(|d| d.to_string().to_lowercase().contains("monitor"))
        {
            return Ok(m);
        }
    }
    host.default_input_device()
        .ok_or_else(|| AudioError::Device("no default input device".into()))
}

/// Captures from a device (system-audio loopback, or a microphone when `loopback` is false).
pub struct DeviceSource {
    ring: Arc<Ring>,
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
    name: String,
}

impl std::fmt::Debug for DeviceSource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DeviceSource")
            .field("name", &self.name)
            .finish_non_exhaustive()
    }
}

impl DeviceSource {
    /// Opens the capture device. `device` is a (case-insensitive) substring of its name.
    pub fn open(
        cfg: &StreamConfig,
        device: Option<&str>,
        loopback: bool,
    ) -> Result<Self, AudioError> {
        let ring = Arc::new(Ring::default());
        let stop = Arc::new(AtomicBool::new(false));
        let (ready_tx, ready_rx) = mpsc::channel::<Result<String, AudioError>>();
        let want = usize::from(cfg.channels);
        let device_name = device.map(str::to_owned);
        let thread = {
            let (ring, stop) = (Arc::clone(&ring), Arc::clone(&stop));
            std::thread::Builder::new()
                .name("nexus-audio-capture".into())
                .spawn(move || {
                    let built = (|| {
                        let host = cpal::default_host();
                        let dev = pick_input_device(&host, device_name.as_deref(), loopback)?;
                        let name = dev.to_string();
                        let (dc, fmt) = if dev.supports_input() {
                            pick_config(&dev, cfg_channels(want), true)?
                        } else {
                            // Loopback on an output device: its output formats describe the stream.
                            pick_config(&dev, cfg_channels(want), false)?
                        };
                        let have = usize::from(dc.channels);
                        let max = MAX_QUEUED_MS * (SAMPLE_RATE as usize / 1000) * want;
                        let err_cb =
                            |e: cpal::Error| warn!(error = %e, "audio capture stream error");
                        let push = move |ring: &Ring, samples: &[f32]| {
                            if let Ok(mut q) = ring.q.lock() {
                                remix(samples, have, want, &mut q);
                                let excess = q.len().saturating_sub(max);
                                if excess > 0 {
                                    q.drain(..excess);
                                }
                                ring.ready.notify_one();
                            }
                        };
                        let r1 = Arc::clone(&ring);
                        let stream = match fmt {
                            SampleFormat::F32 => dev.build_input_stream(
                                dc,
                                move |d: &[f32], _: &_| push(&r1, d),
                                err_cb,
                                None,
                            ),
                            _ => dev.build_input_stream(
                                dc,
                                move |d: &[i16], _: &_| {
                                    let f: Vec<f32> =
                                        d.iter().map(|s| f32::from(*s) / 32768.0).collect();
                                    push(&r1, &f);
                                },
                                err_cb,
                                None,
                            ),
                        }
                        .map_err(dev_err)?;
                        stream.play().map_err(dev_err)?;
                        Ok::<_, AudioError>((stream, name))
                    })();
                    match built {
                        Ok((stream, name)) => {
                            let _ = ready_tx.send(Ok(name));
                            while !stop.load(Ordering::SeqCst) {
                                std::thread::sleep(Duration::from_millis(50));
                            }
                            drop(stream);
                        }
                        Err(e) => {
                            let _ = ready_tx.send(Err(e));
                        }
                    }
                })
                .map_err(dev_err)?
        };
        let name = ready_rx
            .recv_timeout(Duration::from_secs(10))
            .map_err(|_| AudioError::Device("timed out opening the audio device".into()))??;
        debug!(device = %name, loopback, "audio capture opened");
        Ok(Self {
            ring,
            stop,
            thread: Some(thread),
            name,
        })
    }
}

const fn cfg_channels(n: usize) -> u8 {
    if n == 1 { 1 } else { 2 }
}

impl AudioSource for DeviceSource {
    fn name(&self) -> &str {
        &self.name
    }

    fn read(&mut self, out: &mut [f32]) -> Result<(), AudioError> {
        let mut q = self.ring.q.lock().map_err(|_| AudioError::Ended)?;
        let mut waited = 0;
        while q.len() < out.len() {
            let (guard, timeout) = self
                .ring
                .ready
                .wait_timeout(q, Duration::from_millis(100))
                .map_err(|_| AudioError::Ended)?;
            q = guard;
            if timeout.timed_out() {
                waited += 1;
                // A quiet device (nothing playing) may deliver no callbacks at all on some
                // backends; feed silence so the stream stays alive and paced.
                if waited >= 1 && q.len() < out.len() {
                    out.fill(0.0);
                    return Ok(());
                }
            }
        }
        let n = out.len();
        for (o, s) in out.iter_mut().zip(q.drain(..n)) {
            *o = s;
        }
        Ok(())
    }
}

impl Drop for DeviceSource {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

/// A running playback stream fed by a [`Player`].
pub struct DevicePlayback {
    player: Arc<Mutex<Player>>,
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
    name: String,
}

impl std::fmt::Debug for DevicePlayback {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DevicePlayback")
            .field("name", &self.name)
            .finish_non_exhaustive()
    }
}

impl DevicePlayback {
    /// Starts playing whatever `player` produces on the default (or named) output device.
    pub fn start(player: Arc<Mutex<Player>>, device: Option<&str>) -> Result<Self, AudioError> {
        let cfg = player.lock().map_err(|_| AudioError::Ended)?.config();
        let want = usize::from(cfg.channels);
        let stop = Arc::new(AtomicBool::new(false));
        let (ready_tx, ready_rx) = mpsc::channel::<Result<String, AudioError>>();
        let device_name = device.map(str::to_owned);
        let thread = {
            let (player, stop) = (Arc::clone(&player), Arc::clone(&stop));
            std::thread::Builder::new()
                .name("nexus-audio-playback".into())
                .spawn(move || {
                    let built = (|| {
                        let host = cpal::default_host();
                        let dev = match &device_name {
                            Some(w) => host
                                .output_devices()
                                .map_err(dev_err)?
                                .find(|d| d.to_string().to_lowercase().contains(&w.to_lowercase()))
                                .ok_or_else(|| {
                                    AudioError::Device(format!("no output device matching \"{w}\""))
                                })?,
                            None => host.default_output_device().ok_or_else(|| {
                                AudioError::Device("no default output device".into())
                            })?,
                        };
                        let name = dev.to_string();
                        let (dc, fmt) = pick_config(&dev, cfg.channels, false)?;
                        let have = usize::from(dc.channels);
                        let err_cb =
                            |e: cpal::Error| warn!(error = %e, "audio playback stream error");
                        // Plays into a scratch of the stream's own channel count.
                        let render =
                            move |player: &Mutex<Player>, scratch: &mut Vec<f32>, frames: usize| {
                                scratch.resize(frames * want, 0.0);
                                if let Ok(mut p) = player.lock() {
                                    p.fill(scratch);
                                } else {
                                    scratch.fill(0.0);
                                }
                            };
                        let p1 = Arc::clone(&player);
                        let mut scratch = Vec::new();
                        let stream = match fmt {
                            SampleFormat::F32 => dev.build_output_stream(
                                dc,
                                move |out: &mut [f32], _: &_| {
                                    render(&p1, &mut scratch, out.len() / have);
                                    expand(&scratch, want, have, out, |s| s);
                                },
                                err_cb,
                                None,
                            ),
                            _ => dev.build_output_stream(
                                dc,
                                move |out: &mut [i16], _: &_| {
                                    render(&p1, &mut scratch, out.len() / have);
                                    #[allow(clippy::cast_possible_truncation)]
                                    expand(&scratch, want, have, out, |s| {
                                        (s.clamp(-1.0, 1.0) * 32767.0) as i16
                                    });
                                },
                                err_cb,
                                None,
                            ),
                        }
                        .map_err(dev_err)?;
                        stream.play().map_err(dev_err)?;
                        Ok::<_, AudioError>((stream, name))
                    })();
                    match built {
                        Ok((stream, name)) => {
                            let _ = ready_tx.send(Ok(name));
                            while !stop.load(Ordering::SeqCst) {
                                std::thread::sleep(Duration::from_millis(50));
                            }
                            drop(stream);
                        }
                        Err(e) => {
                            let _ = ready_tx.send(Err(e));
                        }
                    }
                })
                .map_err(dev_err)?
        };
        let name = ready_rx
            .recv_timeout(Duration::from_secs(10))
            .map_err(|_| AudioError::Device("timed out opening the playback device".into()))??;
        debug!(device = %name, "audio playback opened");
        Ok(Self {
            player,
            stop,
            thread: Some(thread),
            name,
        })
    }

    /// The shared player (push received datagrams into it).
    #[must_use]
    pub fn player(&self) -> &Arc<Mutex<Player>> {
        &self.player
    }

    /// The device in use.
    #[must_use]
    pub fn device_name(&self) -> &str {
        &self.name
    }
}

/// Copies `want`-channel samples to a `have`-channel device buffer, converting the sample type.
fn expand<T: Copy + Default>(
    src: &[f32],
    want: usize,
    have: usize,
    out: &mut [T],
    conv: impl Fn(f32) -> T,
) {
    for (i, frame) in out.chunks_mut(have).enumerate() {
        for (c, o) in frame.iter_mut().enumerate() {
            let s = src.get(i * want + c.min(want - 1)).copied().unwrap_or(0.0);
            *o = conv(s);
        }
    }
}

impl Drop for DevicePlayback {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

#[cfg(test)]
#[allow(clippy::cast_possible_truncation)]
mod tests {
    use super::*;

    #[test]
    fn remix_between_channel_counts() {
        let mut q = VecDeque::new();
        remix(&[1.0, 3.0, 5.0, 7.0], 2, 1, &mut q);
        assert_eq!(q.iter().copied().collect::<Vec<_>>(), vec![2.0, 6.0]);
        q.clear();
        remix(&[1.0, 2.0], 1, 2, &mut q);
        assert_eq!(
            q.iter().copied().collect::<Vec<_>>(),
            vec![1.0, 1.0, 2.0, 2.0]
        );
        q.clear();
        remix(&[1.0, 2.0, 9.0, 3.0, 4.0, 9.0], 3, 2, &mut q);
        assert_eq!(
            q.iter().copied().collect::<Vec<_>>(),
            vec![1.0, 2.0, 3.0, 4.0]
        );
    }

    #[test]
    fn expand_pads_and_converts() {
        let mut out = [0i16; 6];
        expand(&[0.5, -0.5], 1, 3, &mut out[..3], |s| (s * 100.0) as i16);
        assert_eq!(&out[..3], &[50, 50, 50]);
    }
}

#[cfg(test)]
mod device_tests {
    use super::*;

    /// CI machines have no sound hardware: opening must fail promptly and cleanly, not hang or
    /// panic. (On a developer machine it succeeds, which is fine too.)
    #[test]
    fn opening_without_hardware_fails_or_works_but_never_hangs() {
        let started = std::time::Instant::now();
        let _ = list_devices();
        let cfg = StreamConfig::default();
        let _ = DeviceSource::open(&cfg, None, true);
        let player = Arc::new(Mutex::new(Player::new(cfg).unwrap()));
        let _ = DevicePlayback::start(player, None);
        assert!(started.elapsed() < Duration::from_secs(25));
    }
}
