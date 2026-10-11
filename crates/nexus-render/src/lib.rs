//! The interactive viewer window.
//!
//! * pictures come from the local agent over IPC and are decoded in this process;
//! * the **remote cursor's shape becomes the local pointer**, so the pointer moves with zero
//!   added latency regardless of video latency (spec §2.1);
//! * keys go out as physical HID usages, so the target's own layout interprets them;
//! * local hotkeys (always with Ctrl+Alt+Shift so they never collide with remote shortcuts):
//!   `Q` disconnect, `F` fullscreen, `P` fit ↔ pixel-perfect.
//!
//! Presentation is CPU-based (`softbuffer`) so it works on every platform and under Xvfb; a
//! `wgpu` presenter for zero-copy GPU paths is Phase 4 work (docs/adr/0012).

#![forbid(unsafe_code)]

mod app;
mod keymap;
pub mod layout;
pub mod session;

pub use app::run;
pub use keymap::hid_for;
pub use session::{UiEvent, ViewerOptions};
