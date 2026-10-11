//! Compact little-endian binary codecs for the hot paths.
//!
//! All multi-byte integers are little-endian. Every message starts with a version byte so the
//! layout can evolve; decoders reject versions they do not know rather than guess.

pub mod audio;
pub mod cursor;
pub mod input;
mod reader;
pub mod video;

pub use reader::Reader;

/// Current layout version of all hot-path messages.
pub const WIRE_VERSION: u8 = 1;

/// Errors produced by the binary codecs.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum WireError {
    /// The input ended before the message did.
    #[error("truncated message: {needed} more byte(s) required")]
    Truncated {
        /// Bytes missing.
        needed: usize,
    },
    /// The layout version is not one we understand.
    #[error("unsupported wire version {0}")]
    BadVersion(u8),
    /// An enumerated field holds an unassigned value.
    #[error("unknown {what} value {value}")]
    Unknown {
        /// Field name.
        what: &'static str,
        /// Offending value.
        value: u32,
    },
    /// A length or count exceeds its protocol cap.
    #[error("{what} too large: {len} > {max}")]
    TooLarge {
        /// Field name.
        what: &'static str,
        /// Actual size.
        len: usize,
        /// Maximum allowed.
        max: usize,
    },
    /// A field has a structurally invalid value.
    #[error("invalid {0}")]
    Invalid(&'static str),
    /// Bytes remained after a complete message.
    #[error("{0} trailing byte(s) after message")]
    Trailing(usize),
}
