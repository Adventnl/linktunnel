//! Remote-desktop media sessions.
//!
//! A session has two ends:
//!
//! * the **target** ([`target::serve`]) captures its screen, encodes it, and streams video, the
//!   cursor and (on request) other data to the controller, while applying the controller's
//!   input;
//! * the **controller** ([`controller::ControllerSession`]) receives that, restores frame order
//!   ([`reorder`]), and reports what arrived so the target can adapt.
//!
//! Decoding and presentation live in [`viewer`], deliberately separate from the network half:
//! the agent runs the network side, and the (less trusted, crash-prone) decoder runs in the
//! viewer process.

#![forbid(unsafe_code)]

pub mod controller;
mod error;
pub mod input_wire;
pub mod ladder;
pub mod rate;
pub mod reorder;
pub mod target;
pub mod viewer;

pub use error::MediaError;
