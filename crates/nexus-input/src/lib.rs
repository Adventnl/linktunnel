//! Input injection on the target machine.
//!
//! The controller sends [`InputEvent`]s (physical HID keys, absolute pointer positions in
//! monitor pixels, scroll, committed text). An [`Injector`] turns them into real OS input:
//!
//! | Backend | Where |
//! |---|---|
//! | `x11` | Linux X11 / Xvfb via the XTest extension |
//! | `macos` | macOS via `CGEvent` (needs the Accessibility permission; type-checked only here) |
//! | `windows_inject` | Windows via `SendInput` (type-checked only here; the SYSTEM helper handles the secure desktop) |
//! | [`recording::RecordingInjector`] | tests |
//!
//! Every injector is wrapped by [`Guarded`], which remembers which keys and buttons are held so
//! [`Guarded::release_all`] can undo them when a session ends or focus is lost: a dropped
//! connection must never leave Shift stuck down on the target.

#![deny(unsafe_code)]

mod error;
mod guard;
pub mod hid;
pub mod keys;
pub mod recording;
pub mod remap;
mod scroll;

#[cfg(target_os = "macos")]
#[allow(unsafe_code)]
pub mod macos;
#[cfg(target_os = "windows")]
#[allow(unsafe_code)]
pub mod windows_inject;
#[cfg(windows)]
#[allow(unsafe_code)]
mod windows_pointer;
#[cfg(target_os = "linux")]
pub mod x11;
pub use error::InputError;
pub use guard::Guarded;
pub use nexus_proto::wire::input::{
    InputEvent, KeyState, Modifiers, MouseButton, ScrollPhase, ScrollUnit, TouchPhase,
};
pub use scroll::ScrollAccumulator;

/// Where a monitor sits in the target's virtual desktop. Absolute pointer events carry a monitor
/// index and monitor-local pixels; the injector needs this to place them.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct MonitorRect {
    /// Monitor index as announced in the monitor list.
    pub index: u8,
    /// Left edge in the virtual desktop, in the OS's own coordinate units (pixels on Windows and
    /// X11, points on macOS).
    pub x: i32,
    /// Top edge in the virtual desktop, same units as `x`.
    pub y: i32,
    /// Width in device pixels (the unit of pointer positions on the wire).
    pub width: u32,
    /// Height in device pixels.
    pub height: u32,
    /// Device pixels per coordinate unit: 1.0 everywhere except macOS Retina (2.0).
    pub scale: f32,
}

/// Turns input events into OS input. One injector serves one session, from one thread.
pub trait Injector: Send {
    /// Backend name for logs.
    fn name(&self) -> &'static str;

    /// Tells the injector where the monitors are (call again when the layout changes).
    fn set_monitors(&mut self, monitors: &[MonitorRect]);

    /// Performs one event. Events the platform cannot express return
    /// [`InputError::Unsupported`], which callers log once and otherwise ignore.
    fn inject(&mut self, event: &InputEvent) -> Result<(), InputError>;

    /// Cancels native contacts/state that are not described by ordinary key/button releases.
    /// The guard calls this on focus loss and disconnect. Backends without contacts need no work.
    fn release_native_state(&mut self) -> Result<(), InputError> {
        Ok(())
    }
}

/// The best injector for this machine.
pub fn default_injector() -> Result<Box<dyn Injector>, InputError> {
    #[cfg(target_os = "linux")]
    {
        return Ok(Box::new(x11::X11Injector::connect(None)?));
    }
    #[cfg(windows)]
    {
        return Ok(Box::new(windows_inject::WindowsInjector::new()));
    }
    #[cfg(target_os = "macos")]
    {
        return Ok(Box::new(macos::MacInjector::new()?));
    }
    #[allow(unreachable_code)]
    Err(InputError::Unsupported(
        "no input backend for this platform".into(),
    ))
}
