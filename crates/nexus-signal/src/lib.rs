//! `nexus-signal`: the signaling, presence and pairing-registry server.
//!
//! **Trust model.** This server is deliberately *untrusted for content*:
//!
//! * it authenticates devices by key possession and ties each login to this server's name;
//! * it stores pairings but only if **both** devices signed them, so it cannot invent trust;
//! * it routes opaque, end-to-end-sealed envelopes and one-time pairing rendezvous traffic;
//! * it never sees screen, input, clipboard or file data (sessions are end-to-end QUIC).
//!
//! What a compromised server *can* do: learn who is paired with whom and when devices are online
//! (metadata), refuse service, drop or delay messages. It cannot read sealed content, impersonate
//! a device, or add a pairing. See `docs/THREAT_MODEL.md`.

#![forbid(unsafe_code)]

pub mod config;
pub mod hub;
pub mod ratelimit;
pub mod server;
mod session;
pub mod store;

pub use config::{Rate, SignalConfig};
pub use hub::Hub;
pub use server::{router, serve};
