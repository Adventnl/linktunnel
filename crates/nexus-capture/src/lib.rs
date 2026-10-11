//! Screen and cursor capture.
//!
//! Backends sit behind three small traits ([`CaptureBackend`], [`Capturer`], [`CursorSource`]) so
//! the streaming code never knows which operating system it runs on:
//!
//! | Backend | Where | Status |
//! |---|---|---|
//! | [`synthetic`] | everywhere | test pattern + flashing probe region for latency benchmarks |
//! | `x11` | Linux (X11, Xvfb) | XDamage-driven, partial updates, XFixes cursor |
//! | `macos` | macOS | ScreenCaptureKit (type-checked only here) |
//! | `windows_capture` | Windows | DXGI Desktop Duplication (type-checked only here) |
//! | `macos` | macOS | ScreenCaptureKit (type-checked only here) |
//!
//! Capture is **damage driven**: `next_frame` returns `None` when nothing changed, so a static
//! desktop costs no encoding and no bandwidth.

#![deny(unsafe_code)]

mod error;
pub mod headless;
mod types;

#[cfg(target_os = "macos")]
#[allow(unsafe_code)]
pub mod macos;
pub mod synthetic;
#[cfg(target_os = "windows")]
#[allow(unsafe_code)]
pub mod windows_capture;
#[cfg(target_os = "linux")]
pub mod x11;

use std::sync::Arc;

pub use error::CaptureError;
pub use nexus_codec::{Frame, FrameFormat, Rect};
pub use types::{
    CaptureBackend, Capturer, CursorBitmap, CursorSample, CursorSource, MonitorInfo, limit_rects,
};

/// The best backend for this machine, or why there is none.
pub fn default_backend() -> Result<Arc<dyn CaptureBackend>, CaptureError> {
    #[cfg(target_os = "linux")]
    {
        return Ok(Arc::new(x11::X11Backend::connect(None)?));
    }
    #[cfg(target_os = "macos")]
    {
        return Ok(Arc::new(macos::MacBackend::new()?));
    }
    #[cfg(target_os = "windows")]
    {
        return Ok(Arc::new(windows_capture::WindowsBackend::new()?));
    }
    #[allow(unreachable_code)]
    Err(CaptureError::Unsupported(
        "no capture backend for this platform".into(),
    ))
}
