//! Test support shared by the capture, input and end-to-end tests.
//!
//! [`Xvfb::start`] launches a private virtual X server and returns `None` (so the test can skip
//! with a message) when the `Xvfb` binary is not installed.

#![allow(
    missing_docs,
    clippy::print_stderr,
    clippy::expect_used,
    clippy::unwrap_used
)]

#[cfg(target_os = "linux")]
mod xvfb;
#[cfg(target_os = "linux")]
pub use xvfb::{Painter, Xvfb};
