//! The interactive viewer window.
//!
//! * pictures come from the local agent over IPC and are decoded in this process;
//! * the **remote cursor's shape becomes the local pointer**, so the pointer moves with zero
//!   added latency regardless of video latency (spec §2.1);
//! * keys go out as physical HID usages, so the target's own layout interprets them;
//! * local hotkeys (always with Ctrl+Alt+Shift so they never collide with remote shortcuts):
//!   `Q` disconnect, `F` fullscreen, `P` fit ↔ pixel-perfect, `M` monitor, `B` target privacy.
//!
//! Presentation uses a native wgpu surface (Metal / D3D12 / Vulkan). Decoded CPU BGRA is
//! uploaded once per frame; scaling happens on the GPU. Software presentation remains available
//! when no GPU adapter exists or `NEXUS_SOFTWARE_VIEWER=1` is set.

#![forbid(unsafe_code)]

mod app;
mod gpu;
mod keymap;
pub mod layout;
pub mod session;

pub use app::run;
pub use keymap::hid_for;
pub use session::{UiEvent, ViewerOptions};
