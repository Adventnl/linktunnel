//! The Nexus wire protocol.
//!
//! Two encodings are used on purpose (spec §4.3):
//!
//! * **Protobuf** ([`v1`]) for the low-rate control plane (hello, RPC, clipboard offers, file
//!   operations, …), length-delimited by [`framing`].
//! * **Compact little-endian binary** ([`wire`]) for the hot paths (video frame headers, audio
//!   packets, input events, cursor updates), where JSON/protobuf parsing cost and allocation
//!   would show up in the latency budget.
//!
//! Every decoder in this crate is total: it returns an error for malformed input and never
//! panics, allocates unboundedly, or reads out of bounds. They are exercised by property tests
//! here and by `cargo-fuzz` targets in `fuzz/`.

#![forbid(unsafe_code)]

pub mod features;
pub mod framing;
pub mod serial;
pub mod signal;
pub mod version;
pub mod wire;

/// Generated protobuf types for package `nexus.v1`.
#[allow(
    missing_docs,
    clippy::all,
    clippy::pedantic,
    clippy::nursery,
    unreachable_pub
)]
pub mod v1 {
    include!(concat!(env!("OUT_DIR"), "/nexus.v1.rs"));
}
