//! TCP port forwarding.
//!
//! The controller listens on a local port; every connection to it becomes one stream to the
//! target, which connects to a service on its side (`localhost:5432`, a dev server, a database)
//! and copies bytes both ways. Only the target's policy decides where connections may go: by
//! default loopback only, so a forward cannot be used to reach other machines on the target's
//! network unless that was explicitly allowed.
//!
//! Wire format of one forward stream, before the raw byte copy begins:
//!
//! ```text
//! controller → target:  "PF1" | host_len:u8 | host (utf-8) | port:u16 BE
//! target → controller:  status:u8   (0 = connected; see [`Status`])
//! ```
//!
//! The crate is generic over `AsyncRead + AsyncWrite` so it is tested without a network session.

#![forbid(unsafe_code)]

mod client;
mod policy;
mod server;

pub use client::{ForwardError, listen_and_forward, open_forward};
pub use policy::{AllowRule, ForwardPolicy};
pub use server::{Active, ForwardHost, serve_stream};

/// Outcome byte sent back to the controller.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum Status {
    /// Connected; bytes follow.
    Ok = 0,
    /// The target's policy does not allow that destination.
    Denied = 1,
    /// The destination refused the connection (nothing listening).
    Refused = 2,
    /// The destination did not answer in time or could not be resolved.
    Unreachable = 3,
    /// The request was malformed.
    BadRequest = 4,
}

impl Status {
    pub(crate) fn from_u8(v: u8) -> Self {
        match v {
            0 => Self::Ok,
            1 => Self::Denied,
            2 => Self::Refused,
            3 => Self::Unreachable,
            _ => Self::BadRequest,
        }
    }
}

pub(crate) const MAGIC: &[u8; 3] = b"PF1";
