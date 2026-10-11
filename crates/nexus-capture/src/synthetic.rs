//! A deterministic fake screen for tests and latency benchmarks.
//!
//! The picture is a text-like background with an optional moving block and a 64×64 **probe
//! region** in the top-left corner whose colour a test flips through [`Probe`]. The glass-to-glass
//! benchmark flips the probe, then measures how long until the flipped colour is visible in a
//! decoded frame on the other end.

use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use bytes::Bytes;
use nexus_codec::{Frame, FrameFormat, Rect};
use nexus_core::clock::now_us;

use crate::CaptureError;
use crate::types::{
    CaptureBackend, Capturer, CursorBitmap, CursorSample, CursorSource, MonitorInfo,
};

/// Side of the square probe region.
pub const PROBE_SIZE: u32 = 64;
const PROBE_PX: u16 = 64;
/// Probe colour when the flash is on (BGR).
pub const PROBE_ON: [u8; 3] = [255, 255, 255];
/// Probe colour when the flash is off (BGR).
pub const PROBE_OFF: [u8; 3] = [0, 0, 0];

/// Settings for the synthetic screen.
#[derive(Debug, Clone)]
pub struct SyntheticConfig {
    /// Width of each monitor.
    pub width: u32,
    /// Height of each monitor.
    pub height: u32,
    /// Frame rate while animating.
    pub fps: f32,
    /// Number of monitors.
    pub monitors: u8,
    /// Move a block across the screen continuously (otherwise the screen only changes when the
    /// probe flips or [`Probe::touch`] is called).
    pub animate: bool,
    /// Percentage (0–100) of the screen height, at the bottom, refilled with fresh noise every
    /// animation frame: an incompressible stand-in for video that saturates any bitrate limit.
    pub noise_percent: u8,
}

impl Default for SyntheticConfig {
    fn default() -> Self {
        Self {
            width: 640,
            height: 360,
            fps: 60.0,
            monitors: 1,
            animate: false,
            noise_percent: 0,
        }
    }
}

#[derive(Debug, Default)]
struct Shared {
    state: Mutex<State>,
    wake: Condvar,
}

#[derive(Debug, Default)]
struct State {
    flash: bool,
    version: u64,
    cursor: (i32, i32),
    cursor_shape: u32,
}

/// Remote control for the fake screen.
#[derive(Debug, Clone)]
pub struct Probe {
    shared: Arc<Shared>,
}

impl Probe {
    /// Turns the probe region white (`true`) or black (`false`); wakes the capturer immediately.
    pub fn set_flash(&self, on: bool) {
        if let Ok(mut s) = self.shared.state.lock() {
            s.flash = on;
            s.version += 1;
        }
        self.shared.wake.notify_all();
    }

    /// Marks the whole screen changed (forces a frame).
    pub fn touch(&self) {
        if let Ok(mut s) = self.shared.state.lock() {
            s.version += 1;
        }
        self.shared.wake.notify_all();
    }

    /// Moves the fake cursor.
    pub fn move_cursor(&self, x: i32, y: i32) {
        if let Ok(mut s) = self.shared.state.lock() {
            s.cursor = (x, y);
        }
    }

    /// Switches the fake cursor's shape.
    pub fn set_cursor_shape(&self, shape_id: u32) {
        if let Ok(mut s) = self.shared.state.lock() {
            s.cursor_shape = shape_id;
        }
    }
}

/// The fake capture backend.
#[derive(Debug)]
pub struct SyntheticBackend {
    config: SyntheticConfig,
    shared: Arc<Shared>,
}

impl SyntheticBackend {
    /// Creates a backend and the probe that drives it.
    #[must_use]
    pub fn new(config: SyntheticConfig) -> (Self, Probe) {
        let shared = Arc::new(Shared::default());
        (
            Self {
                config,
                shared: Arc::clone(&shared),
            },
            Probe { shared },
        )
    }
}

impl CaptureBackend for SyntheticBackend {
    fn name(&self) -> &'static str {
        "synthetic"
    }

    fn monitors(&self) -> Result<Vec<MonitorInfo>, CaptureError> {
        Ok((0..self.config.monitors)
            .map(|i| MonitorInfo {
                index: i,
                name: format!("Synthetic-{i}"),
                x: i32::from(i) * i32::try_from(self.config.width).unwrap_or(0),
                y: 0,
                width: self.config.width,
                height: self.config.height,
                scale: 1.0,
                refresh_hz: self.config.fps,
                primary: i == 0,
            })
            .collect())
    }

    fn open(&self, index: u8) -> Result<Box<dyn Capturer>, CaptureError> {
        let monitor = self
            .monitors()?
            .into_iter()
            .find(|m| m.index == index)
            .ok_or(CaptureError::NoSuchMonitor(index))?;
        Ok(Box::new(SyntheticCapturer::new(
            monitor,
            self.config.clone(),
            Arc::clone(&self.shared),
        )))
    }

    fn cursor_source(&self) -> Result<Option<Box<dyn CursorSource>>, CaptureError> {
        Ok(Some(Box::new(SyntheticCursor {
            shared: Arc::clone(&self.shared),
            announced: None,
        })))
    }
}

struct SyntheticCapturer {
    monitor: MonitorInfo,
    config: SyntheticConfig,
    shared: Arc<Shared>,
    canvas: Vec<u8>,
    seen_version: u64,
    tick: u64,
    next_tick_at: Instant,
    first: bool,
    block: Option<Rect>,
}

const BLOCK: u32 = 32;
const BLOCK_PX: u16 = 32;

impl SyntheticCapturer {
    fn new(monitor: MonitorInfo, config: SyntheticConfig, shared: Arc<Shared>) -> Self {
        let (w, h) = (monitor.width as usize, monitor.height as usize);
        let mut canvas = vec![255u8; w * h * 4];
        // "Text": dark dashes on white, deterministic.
        for y in 0..h {
            for x in 0..w {
                if (x / 7 + y / 12) % 3 == 0 && y % 12 < 8 && x % 7 < 5 && y > PROBE_SIZE as usize {
                    let i = (y * w + x) * 4;
                    canvas[i..i + 3].copy_from_slice(&[24, 24, 24]);
                }
            }
        }
        let mut c = Self {
            monitor,
            config,
            shared,
            canvas,
            seen_version: 0,
            tick: 0,
            next_tick_at: Instant::now(),
            first: true,
            block: None,
        };
        c.paint_probe(false);
        c
    }

    fn paint_probe(&mut self, on: bool) {
        let colour = if on { PROBE_ON } else { PROBE_OFF };
        let w = self.monitor.width as usize;
        for y in 0..PROBE_SIZE as usize {
            for x in 0..PROBE_SIZE as usize {
                let i = (y * w + x) * 4;
                self.canvas[i..i + 3].copy_from_slice(&colour);
                self.canvas[i + 3] = 255;
            }
        }
    }

    fn fill(&mut self, r: Rect, colour: [u8; 3]) {
        let w = self.monitor.width as usize;
        for y in usize::from(r.y)..usize::from(r.y + r.h) {
            for x in usize::from(r.x)..usize::from(r.x + r.w) {
                let i = (y * w + x) * 4;
                self.canvas[i..i + 3].copy_from_slice(&colour);
            }
        }
    }

    /// Refills the noise band with pseudo-random pixels and returns its rectangle.
    fn noise(&mut self) -> Option<Rect> {
        let pct = u32::from(self.config.noise_percent.min(100));
        if pct == 0 {
            return None;
        }
        let (w, h) = (self.monitor.width, self.monitor.height);
        let band = h * pct / 100;
        let top = h - band;
        let mut state = self.tick.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1;
        for y in top..h {
            for x in 0..w {
                state ^= state << 13;
                state ^= state >> 7;
                state ^= state << 17;
                let i = ((y * w + x) * 4) as usize;
                self.canvas[i..i + 3].copy_from_slice(&state.to_le_bytes()[..3]);
            }
        }
        Some(Rect {
            x: 0,
            y: u16::try_from(top).ok()?,
            w: u16::try_from(w).ok()?,
            h: u16::try_from(band).ok()?,
        })
    }

    fn frame(&self, dirty: Vec<Rect>) -> Frame {
        Frame {
            width: self.monitor.width,
            height: self.monitor.height,
            stride: self.monitor.width as usize * 4,
            format: FrameFormat::Bgra8,
            data: Bytes::copy_from_slice(&self.canvas),
            captured_us: now_us(),
            dirty,
        }
    }

    fn lock(&self) -> Result<std::sync::MutexGuard<'_, State>, CaptureError> {
        self.shared
            .state
            .lock()
            .map_err(|_| CaptureError::Failed("probe lock poisoned".into()))
    }
}

impl Capturer for SyntheticCapturer {
    fn name(&self) -> &'static str {
        "synthetic"
    }

    fn monitor(&self) -> &MonitorInfo {
        &self.monitor
    }

    fn next_frame(&mut self, timeout: Duration) -> Result<Option<Frame>, CaptureError> {
        if self.first {
            self.first = false;
            self.next_tick_at = Instant::now();
            return Ok(Some(self.frame(Vec::new())));
        }
        let deadline = Instant::now() + timeout;
        loop {
            let (flash, version) = {
                let s = self.lock()?;
                (s.flash, s.version)
            };
            let animating = self.config.animate && Instant::now() >= self.next_tick_at;
            if version != self.seen_version || animating {
                let mut dirty = Vec::new();
                if version != self.seen_version {
                    self.seen_version = version;
                    self.paint_probe(flash);
                    dirty.push(Rect {
                        x: 0,
                        y: 0,
                        w: PROBE_PX,
                        h: PROBE_PX,
                    });
                }
                if animating {
                    self.tick += 1;
                    self.next_tick_at += Duration::from_secs_f32(1.0 / self.config.fps);
                    if let Some(old) = self.block.take() {
                        // Restore the background by repainting white; the dashes under the block
                        // are not important for a test pattern.
                        self.fill(old, [255, 255, 255]);
                        dirty.push(old);
                    }
                    let span = self.monitor.width.saturating_sub(BLOCK + 1).max(1);
                    let x = u16::try_from((self.tick * 4) % u64::from(span)).unwrap_or(0);
                    let y = u16::try_from(PROBE_SIZE + 16).unwrap_or(80);
                    let r = Rect {
                        x,
                        y,
                        w: BLOCK_PX,
                        h: BLOCK_PX,
                    };
                    self.fill(r, [200, 60, 30]);
                    self.block = Some(r);
                    dirty.push(r);
                    if let Some(noise) = self.noise() {
                        dirty.push(noise);
                    }
                }
                return Ok(Some(self.frame(dirty)));
            }
            let now = Instant::now();
            if now >= deadline {
                return Ok(None);
            }
            let mut wait = deadline - now;
            if self.config.animate {
                wait = wait.min(self.next_tick_at.saturating_duration_since(now));
            }
            let guard = self.lock()?;
            if guard.version == self.seen_version {
                let _ = self
                    .shared
                    .wake
                    .wait_timeout(guard, wait.max(Duration::from_micros(200)));
            }
        }
    }
}

struct SyntheticCursor {
    shared: Arc<Shared>,
    announced: Option<u32>,
}

impl CursorSource for SyntheticCursor {
    fn poll(&mut self) -> Result<CursorSample, CaptureError> {
        let (pos, shape) = {
            let s = self
                .shared
                .state
                .lock()
                .map_err(|_| CaptureError::Failed("probe lock poisoned".into()))?;
            (s.cursor, s.cursor_shape)
        };
        let new_shape = (self.announced != Some(shape)).then(|| {
            let size = 12u32;
            let mut px = vec![0u8; (size * size * 4) as usize];
            for (i, p) in px.chunks_exact_mut(4).enumerate() {
                // An arrow-ish triangle whose tint depends on the shape id.
                let i = u32::try_from(i).unwrap_or(0);
                let (x, y) = (i % size, i / size);
                if x <= y {
                    p.copy_from_slice(&[u8::try_from(shape % 256).unwrap_or(0), 0, 255, 255]);
                }
            }
            CursorBitmap {
                shape_id: shape,
                width: size,
                height: size,
                hot_x: 0,
                hot_y: 0,
                bgra: Bytes::from(px),
            }
        });
        self.announced = Some(shape);
        Ok(CursorSample {
            x: pos.0,
            y: pos.1,
            visible: true,
            shape_id: shape,
            new_shape,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn probe_pixel(f: &Frame) -> [u8; 3] {
        [f.data[0], f.data[1], f.data[2]]
    }

    #[test]
    fn first_frame_is_full_then_idle_screens_produce_nothing() {
        let (backend, _probe) = SyntheticBackend::new(SyntheticConfig::default());
        let mut cap = backend.open(0).unwrap();
        let first = cap.next_frame(Duration::from_millis(10)).unwrap().unwrap();
        assert_eq!((first.width, first.height), (640, 360));
        assert!(
            first.dirty.is_empty(),
            "first frame has no damage list: everything is new"
        );
        first.validate().unwrap();
        assert!(cap.next_frame(Duration::from_millis(20)).unwrap().is_none());
    }

    #[test]
    fn flipping_the_probe_wakes_the_capturer_with_a_dirty_rect() {
        let (backend, probe) = SyntheticBackend::new(SyntheticConfig::default());
        let mut cap = backend.open(0).unwrap();
        let first = cap.next_frame(Duration::from_millis(10)).unwrap().unwrap();
        assert_eq!(probe_pixel(&first), PROBE_OFF);
        let waiter =
            std::thread::spawn(move || cap.next_frame(Duration::from_secs(2)).unwrap().unwrap());
        std::thread::sleep(Duration::from_millis(30));
        probe.set_flash(true);
        let frame = waiter.join().unwrap();
        assert_eq!(probe_pixel(&frame), PROBE_ON);
        assert_eq!(
            frame.dirty,
            vec![Rect {
                x: 0,
                y: 0,
                w: 64,
                h: 64
            }]
        );
    }

    #[test]
    fn animation_runs_at_the_configured_rate() {
        let (backend, _probe) = SyntheticBackend::new(SyntheticConfig {
            animate: true,
            fps: 100.0,
            ..Default::default()
        });
        let mut cap = backend.open(0).unwrap();
        let _ = cap.next_frame(Duration::from_millis(10)).unwrap();
        let started = Instant::now();
        let mut n = 0;
        while started.elapsed() < Duration::from_millis(300) {
            if cap.next_frame(Duration::from_millis(50)).unwrap().is_some() {
                n += 1;
            }
        }
        assert!(
            (20..=40).contains(&n),
            "expected ~30 frames in 300 ms at 100 fps, got {n}"
        );
    }

    #[test]
    fn monitors_and_bad_index() {
        let (backend, _p) = SyntheticBackend::new(SyntheticConfig {
            monitors: 2,
            ..Default::default()
        });
        let m = backend.monitors().unwrap();
        assert_eq!(m.len(), 2);
        assert_eq!(m[1].x, 640);
        assert!(matches!(
            backend.open(5),
            Err(CaptureError::NoSuchMonitor(5))
        ));
    }

    #[test]
    fn cursor_reports_shape_only_once() {
        let (backend, probe) = SyntheticBackend::new(SyntheticConfig::default());
        let mut cur = backend.cursor_source().unwrap().unwrap();
        let a = cur.poll().unwrap();
        assert!(a.new_shape.is_some());
        probe.move_cursor(10, 20);
        let b = cur.poll().unwrap();
        assert_eq!((b.x, b.y), (10, 20));
        assert!(b.new_shape.is_none());
        probe.set_cursor_shape(3);
        assert_eq!(cur.poll().unwrap().new_shape.unwrap().shape_id, 3);
    }
}
