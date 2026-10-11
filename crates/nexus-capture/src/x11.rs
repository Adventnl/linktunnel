//! X11 capture (also used with Xvfb for headless servers).
//!
//! * Pictures come from `GetImage` on the root window, restricted to the damaged bounding box and
//!   composited into a persistent canvas, so a blinking text cursor costs a few hundred bytes of
//!   X11 traffic rather than a full screen.
//! * Damage comes from the XDamage extension (event driven: an idle desktop wakes nothing).
//! * The cursor comes from XFixes (`GetCursorImage`), which also reports position.
//!
//! Wayland sessions need the PipeWire/portal backend instead (see docs/LIMITATIONS.md).

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::time::{Duration, Instant};

use bytes::Bytes;
use nexus_codec::{Frame, FrameFormat, Rect};
use nexus_core::clock::now_us;
use x11rb::connection::Connection;
use x11rb::protocol::Event;
use x11rb::protocol::damage::{self, ConnectionExt as _};
use x11rb::protocol::randr::ConnectionExt as _;
use x11rb::protocol::xfixes::ConnectionExt as _;
use x11rb::protocol::xproto::{self, ConnectionExt as _, ImageFormat};
use x11rb::rust_connection::RustConnection;

use crate::CaptureError;
use crate::types::{
    CaptureBackend, Capturer, CursorBitmap, CursorSample, CursorSource, MonitorInfo, limit_rects,
};

/// Most dirty rectangles reported per frame (matches the wire format's limit).
const MAX_RECTS: usize = 64;

fn failed(e: impl std::fmt::Display) -> CaptureError {
    CaptureError::Failed(e.to_string())
}

/// Connects to an X server.
pub struct X11Backend {
    conn: Arc<RustConnection>,
    root: xproto::Window,
    screen_w: u16,
    screen_h: u16,
    display: String,
}

impl std::fmt::Debug for X11Backend {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("X11Backend")
            .field("display", &self.display)
            .finish_non_exhaustive()
    }
}

impl X11Backend {
    /// Connects to `display` (e.g. `:0`), or `$DISPLAY` when `None`.
    pub fn connect(display: Option<&str>) -> Result<Self, CaptureError> {
        let name = display
            .map(str::to_owned)
            .or_else(|| std::env::var("DISPLAY").ok());
        let (conn, screen_num) = RustConnection::connect(name.as_deref()).map_err(|e| {
            CaptureError::NoDisplay(format!(
                "cannot connect to X display {}: {e}",
                name.as_deref().unwrap_or("(DISPLAY unset)")
            ))
        })?;
        let screen = conn
            .setup()
            .roots
            .get(screen_num)
            .ok_or_else(|| CaptureError::NoDisplay("X server reported no screens".into()))?;
        let (root, screen_w, screen_h, depth) = (
            screen.root,
            screen.width_in_pixels,
            screen.height_in_pixels,
            screen.root_depth,
        );
        // We only handle the universal case: 32 bits per pixel, little-endian, B-G-R-x.
        let setup = conn.setup();
        let fmt_ok = setup
            .pixmap_formats
            .iter()
            .any(|f| f.depth == depth && f.bits_per_pixel == 32);
        let visual_ok = screen
            .allowed_depths
            .iter()
            .filter(|d| d.depth == depth)
            .flat_map(|d| d.visuals.iter())
            .any(|v| {
                v.visual_id == screen.root_visual
                    && v.red_mask == 0x00ff_0000
                    && v.green_mask == 0x0000_ff00
                    && v.blue_mask == 0x0000_00ff
            });
        if !fmt_ok || !visual_ok || setup.image_byte_order != xproto::ImageOrder::LSB_FIRST {
            return Err(CaptureError::Unsupported(format!(
                "X server pixel format not supported (depth {depth}); need 24/32-bit BGRX little-endian"
            )));
        }
        Ok(Self {
            conn: Arc::new(conn),
            root,
            screen_w,
            screen_h,
            display: name.unwrap_or_default(),
        })
    }
}

impl CaptureBackend for X11Backend {
    fn name(&self) -> &'static str {
        "x11"
    }

    fn monitors(&self) -> Result<Vec<MonitorInfo>, CaptureError> {
        let mut out = Vec::new();
        if let Ok(reply) = self
            .conn
            .randr_get_monitors(self.root, true)
            .map_err(failed)?
            .reply()
        {
            for (i, m) in reply.monitors.iter().enumerate() {
                let name = self
                    .conn
                    .get_atom_name(m.name)
                    .ok()
                    .and_then(|c| c.reply().ok())
                    .map_or_else(
                        || format!("monitor-{i}"),
                        |r| String::from_utf8_lossy(&r.name).into_owned(),
                    );
                out.push(MonitorInfo {
                    index: u8::try_from(i).map_err(failed)?,
                    name,
                    x: i32::from(m.x),
                    y: i32::from(m.y),
                    width: u32::from(m.width),
                    height: u32::from(m.height),
                    scale: 1.0,
                    refresh_hz: 0.0,
                    primary: m.primary,
                });
            }
        }
        if out.is_empty() {
            out.push(MonitorInfo {
                index: 0,
                name: "screen".into(),
                x: 0,
                y: 0,
                width: u32::from(self.screen_w),
                height: u32::from(self.screen_h),
                scale: 1.0,
                refresh_hz: 0.0,
                primary: true,
            });
        }
        Ok(out)
    }

    fn open(&self, index: u8) -> Result<Box<dyn Capturer>, CaptureError> {
        let monitor = self
            .monitors()?
            .into_iter()
            .find(|m| m.index == index)
            .ok_or(CaptureError::NoSuchMonitor(index))?;
        Ok(Box::new(X11Capturer::new(
            Arc::clone(&self.conn),
            self.root,
            monitor,
            (self.screen_w, self.screen_h),
        )?))
    }

    fn cursor_source(&self) -> Result<Option<Box<dyn CursorSource>>, CaptureError> {
        // A connection of its own: the capture thread and the cursor thread then never queue
        // behind each other's round trips.
        let (conn, _) = RustConnection::connect(
            Some(&self.display)
                .filter(|d| !d.is_empty())
                .map(String::as_str),
        )
        .map_err(failed)?;
        let version = conn
            .xfixes_query_version(4, 0)
            .map_err(failed)?
            .reply()
            .map_err(failed)?;
        if version.major_version < 2 {
            return Ok(None);
        }
        Ok(Some(Box::new(X11Cursor {
            conn,
            last_serial: None,
        })))
    }
}

struct X11Capturer {
    conn: Arc<RustConnection>,
    root: xproto::Window,
    monitor: MonitorInfo,
    canvas: Vec<u8>,
    damage_rx: Receiver<Rect>,
    stop: Arc<AtomicBool>,
    waker: xproto::Window,
    first: bool,
    root_size: (u16, u16),
    last_geometry_check: Instant,
}

impl X11Capturer {
    fn new(
        conn: Arc<RustConnection>,
        root: xproto::Window,
        monitor: MonitorInfo,
        root_size: (u16, u16),
    ) -> Result<Self, CaptureError> {
        conn.damage_query_version(1, 1)
            .map_err(failed)?
            .reply()
            .map_err(failed)?;
        let damage_id = conn.generate_id().map_err(failed)?;
        conn.damage_create(damage_id, root, damage::ReportLevel::RAW_RECTANGLES)
            .map_err(failed)?;
        // A window of our own that we can send an event to, to unblock the reader thread.
        let waker = conn.generate_id().map_err(failed)?;
        conn.create_window(
            0,
            waker,
            root,
            0,
            0,
            1,
            1,
            0,
            xproto::WindowClass::INPUT_ONLY,
            0,
            &xproto::CreateWindowAux::new(),
        )
        .map_err(failed)?;
        conn.flush().map_err(failed)?;

        let (tx, damage_rx) = mpsc::channel();
        let stop = Arc::new(AtomicBool::new(false));
        {
            let (conn, stop) = (Arc::clone(&conn), Arc::clone(&stop));
            let (mx, my, mw, mh) = (monitor.x, monitor.y, monitor.width, monitor.height);
            std::thread::Builder::new()
                .name("x11-damage".into())
                .spawn(move || {
                    while !stop.load(Ordering::Relaxed) {
                        match conn.wait_for_event() {
                            Ok(Event::DamageNotify(ev)) => {
                                if let Some(rect) = clip(
                                    ev.area.x.into(),
                                    ev.area.y.into(),
                                    ev.area.width,
                                    ev.area.height,
                                    mx,
                                    my,
                                    mw,
                                    mh,
                                ) && tx.send(rect).is_err()
                                {
                                    return;
                                }
                            }
                            Ok(_) => {}
                            Err(_) => return,
                        }
                    }
                })
                .map_err(failed)?;
        }
        let canvas = vec![0u8; monitor.width as usize * monitor.height as usize * 4];
        Ok(Self {
            conn,
            root,
            monitor,
            canvas,
            damage_rx,
            stop,
            waker,
            first: true,
            root_size,
            last_geometry_check: Instant::now(),
        })
    }

    /// Copies `rect` (monitor coordinates) from the screen into the canvas.
    fn grab(&mut self, rect: Rect) -> Result<(), CaptureError> {
        let (w, h) = (usize::from(rect.w), usize::from(rect.h));
        let reply = self
            .conn
            .get_image(
                ImageFormat::Z_PIXMAP,
                self.root,
                i16::try_from(self.monitor.x + i32::from(rect.x)).map_err(failed)?,
                i16::try_from(self.monitor.y + i32::from(rect.y)).map_err(failed)?,
                rect.w,
                rect.h,
                !0,
            )
            .map_err(failed)?
            .reply()
            .map_err(failed)?;
        if reply.data.len() < w * h * 4 {
            return Err(failed(format!(
                "short GetImage reply: {} < {}",
                reply.data.len(),
                w * h * 4
            )));
        }
        let stride = self.monitor.width as usize * 4;
        for row in 0..h {
            let dst = (usize::from(rect.y) + row) * stride + usize::from(rect.x) * 4;
            let src = row * w * 4;
            self.canvas[dst..dst + w * 4].copy_from_slice(&reply.data[src..src + w * 4]);
        }
        Ok(())
    }

    fn check_geometry(&mut self) -> Result<(), CaptureError> {
        if self.last_geometry_check.elapsed() < Duration::from_secs(1) {
            return Ok(());
        }
        self.last_geometry_check = Instant::now();
        let g = self
            .conn
            .get_geometry(self.root)
            .map_err(failed)?
            .reply()
            .map_err(failed)?;
        if (g.width, g.height) != self.root_size {
            return Err(CaptureError::Failed(format!(
                "display size changed from {}x{} to {}x{}",
                self.root_size.0, self.root_size.1, g.width, g.height
            )));
        }
        Ok(())
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
}

#[allow(clippy::too_many_arguments)]
fn clip(x: i32, y: i32, w: u16, h: u16, mx: i32, my: i32, mw: u32, mh: u32) -> Option<Rect> {
    let x0 = x.max(mx);
    let y0 = y.max(my);
    let x1 = (x + i32::from(w)).min(mx + i32::try_from(mw).ok()?);
    let y1 = (y + i32::from(h)).min(my + i32::try_from(mh).ok()?);
    if x1 <= x0 || y1 <= y0 {
        return None;
    }
    Some(Rect {
        x: u16::try_from(x0 - mx).ok()?,
        y: u16::try_from(y0 - my).ok()?,
        w: u16::try_from(x1 - x0).ok()?,
        h: u16::try_from(y1 - y0).ok()?,
    })
}

impl Capturer for X11Capturer {
    fn name(&self) -> &'static str {
        "x11"
    }

    fn monitor(&self) -> &MonitorInfo {
        &self.monitor
    }

    fn next_frame(&mut self, timeout: Duration) -> Result<Option<Frame>, CaptureError> {
        self.check_geometry()?;
        if self.first {
            self.first = false;
            // Discard damage that predates the full grab. A round trip guarantees the server has
            // delivered everything it queued for us; the short sleep lets the reader thread
            // hand those events over.
            self.conn
                .get_input_focus()
                .map_err(failed)?
                .reply()
                .map_err(failed)?;
            std::thread::sleep(Duration::from_millis(3));
            while self.damage_rx.try_recv().is_ok() {}
            let full = Rect {
                x: 0,
                y: 0,
                w: u16::try_from(self.monitor.width).map_err(failed)?,
                h: u16::try_from(self.monitor.height).map_err(failed)?,
            };
            self.grab(full)?;
            return Ok(Some(self.frame(Vec::new())));
        }
        let first = match self.damage_rx.recv_timeout(timeout) {
            Ok(r) => r,
            Err(RecvTimeoutError::Timeout) => return Ok(None),
            Err(RecvTimeoutError::Disconnected) => return Err(failed("X connection closed")),
        };
        // Let a burst of rectangles (a window redraw) arrive together.
        let mut rects = vec![first];
        let settle = Instant::now() + Duration::from_micros(500);
        loop {
            match self.damage_rx.try_recv() {
                Ok(r) => rects.push(r),
                Err(_) if Instant::now() < settle => std::thread::sleep(Duration::from_micros(100)),
                Err(_) => break,
            }
        }
        let rects = limit_rects(rects, MAX_RECTS);
        for r in &rects {
            self.grab(*r)?;
        }
        Ok(Some(self.frame(rects)))
    }
}

impl Drop for X11Capturer {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        // Wake the reader thread: an event with an empty mask goes to the window's creator (us).
        let ev =
            xproto::ClientMessageEvent::new(32, self.waker, xproto::AtomEnum::WM_NAME, [0u32; 5]);
        let _ = self
            .conn
            .send_event(false, self.waker, xproto::EventMask::NO_EVENT, ev);
        let _ = self.conn.flush();
    }
}

struct X11Cursor {
    conn: RustConnection,
    last_serial: Option<u32>,
}

impl CursorSource for X11Cursor {
    fn poll(&mut self) -> Result<CursorSample, CaptureError> {
        let img = self
            .conn
            .xfixes_get_cursor_image()
            .map_err(failed)?
            .reply()
            .map_err(failed)?;
        let new_shape = (self.last_serial != Some(img.cursor_serial)).then(|| {
            let mut bgra = Vec::with_capacity(img.cursor_image.len() * 4);
            for px in &img.cursor_image {
                bgra.extend_from_slice(&px.to_le_bytes()); // ARGB u32 → B,G,R,A in memory
            }
            CursorBitmap {
                shape_id: img.cursor_serial,
                width: u32::from(img.width),
                height: u32::from(img.height),
                hot_x: u32::from(img.xhot),
                hot_y: u32::from(img.yhot),
                bgra: Bytes::from(bgra),
            }
        });
        self.last_serial = Some(img.cursor_serial);
        Ok(CursorSample {
            x: i32::from(img.x),
            y: i32::from(img.y),
            visible: true,
            shape_id: img.cursor_serial,
            new_shape,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clipping() {
        // Fully inside.
        assert_eq!(
            clip(10, 10, 5, 5, 0, 0, 100, 100),
            Some(Rect {
                x: 10,
                y: 10,
                w: 5,
                h: 5
            })
        );
        // Straddling the left/top edge of a monitor at (100, 0).
        assert_eq!(
            clip(90, 0, 20, 5, 100, 0, 50, 50),
            Some(Rect {
                x: 0,
                y: 0,
                w: 10,
                h: 5
            })
        );
        // Outside entirely.
        assert_eq!(clip(0, 0, 10, 10, 100, 0, 50, 50), None);
        // Overhanging the far edge.
        assert_eq!(
            clip(140, 40, 30, 30, 100, 0, 50, 50),
            Some(Rect {
                x: 40,
                y: 40,
                w: 10,
                h: 10
            })
        );
    }
}
