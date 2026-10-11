use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::{Duration, Instant};

use x11rb::connection::Connection;
use x11rb::protocol::xproto::{self, ConnectionExt as _};
use x11rb::rust_connection::RustConnection;

/// A private X server that is killed when dropped.
pub struct Xvfb {
    child: Child,
    display: String,
}

impl Xvfb {
    /// Starts `Xvfb` with a `width`×`height` 24-bit screen. `None` if it is not installed.
    #[must_use]
    pub fn start(width: u32, height: u32) -> Option<Self> {
        // Tests in one process run in parallel: a process-wide counter keeps them apart.
        static NEXT: AtomicU32 = AtomicU32::new(0);
        let base = 200 + (std::process::id() % 600);
        for _ in 0..40 {
            let n = base + NEXT.fetch_add(1, Ordering::SeqCst);
            if std::path::Path::new(&format!("/tmp/.X11-unix/X{n}")).exists()
                || std::path::Path::new(&format!("/tmp/.X{n}-lock")).exists()
            {
                continue;
            }
            let display = format!(":{n}");
            let child = Command::new("Xvfb")
                .args([
                    &display,
                    "-screen",
                    "0",
                    &format!("{width}x{height}x24"),
                    "-nolisten",
                    "tcp",
                    "-noreset",
                ])
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn();
            let Ok(child) = child else {
                eprintln!("Xvfb is not installed; skipping X11 test");
                return None;
            };
            let mut x = Self { child, display };
            let deadline = Instant::now() + Duration::from_secs(10);
            while Instant::now() < deadline {
                if x.child.try_wait().ok().flatten().is_some() {
                    break; // the display number was taken; try the next one
                }
                if RustConnection::connect(Some(&x.display)).is_ok() {
                    return Some(x);
                }
                std::thread::sleep(Duration::from_millis(50));
            }
        }
        eprintln!("could not start Xvfb");
        None
    }

    /// The display name, e.g. `:213`.
    #[must_use]
    pub fn display(&self) -> &str {
        &self.display
    }
}

impl Drop for Xvfb {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        let n = self.display.trim_start_matches(':');
        let _ = std::fs::remove_file(format!("/tmp/.X11-unix/X{n}"));
        let _ = std::fs::remove_file(format!("/tmp/.X{n}-lock"));
    }
}

/// Draws solid rectangles on the root window of a display, like a very small application.
pub struct Painter {
    conn: RustConnection,
    root: xproto::Window,
    gc: xproto::Gcontext,
}

impl Painter {
    pub fn connect(display: &str) -> Self {
        let (conn, screen) = RustConnection::connect(Some(display)).expect("connect to X");
        let root = conn.setup().roots[screen].root;
        let gc = conn.generate_id().expect("gc id");
        conn.create_gc(gc, root, &xproto::CreateGCAux::new())
            .expect("create gc");
        Self { conn, root, gc }
    }

    /// Fills a rectangle with `0xRRGGBB`.
    pub fn fill(&self, x: i16, y: i16, w: u16, h: u16, rgb: u32) {
        self.conn
            .change_gc(self.gc, &xproto::ChangeGCAux::new().foreground(rgb))
            .expect("change gc");
        self.conn
            .poly_fill_rectangle(
                self.root,
                self.gc,
                &[xproto::Rectangle {
                    x,
                    y,
                    width: w,
                    height: h,
                }],
            )
            .expect("fill");
        self.conn.flush().expect("flush");
    }

    /// Warps the pointer.
    pub fn warp_pointer(&self, x: i16, y: i16) {
        self.conn
            .warp_pointer(x11rb::NONE, self.root, 0, 0, 0, 0, x, y)
            .expect("warp");
        self.conn.flush().expect("flush");
    }
}
