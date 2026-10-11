//! End-to-end encrypted transport between paired devices.
//!
//! * **Identity = transport identity.** The QUIC endpoint is keyed with the device's Ed25519
//!   identity key, and TLS 1.3 raw-public-key authentication proves to each side exactly which
//!   key it is talking to.
//! * **Zero trust at the door.** Only keys in the [`AllowList`] (derived from the dual-signed
//!   pairing records) can complete a connection; everything else is closed right after the
//!   handshake, before any application data is parsed. Revoking a key closes its live sessions.
//! * **Path-aware.** Sessions start on whatever works (LAN, hole-punched, relay) and migrate to
//!   better paths; [`Session::path`] reports LAN / direct / relay for the UI.
//! * **No hidden third parties.** Relays are off unless the owner configures self-hosted ones
//!   (or explicitly opts into public ones).

#![forbid(unsafe_code)]

mod allow;
mod config;
pub mod diag;
mod error;
mod node;
pub mod pathkind;
mod session;
mod stream;

pub use allow::{AllowList, CLOSE_NORMAL, CLOSE_NOT_PAIRED, CLOSE_REVOKED, SESSION_ALPN};
pub use config::{NodeConfig, RelayServer, RelaySetting, RelayTrust};
pub use error::NetError;
pub use node::{Node, Reachability};
pub use session::{Established, PathInfo, PeerAddr, Session, SessionRole};
pub use stream::{FramedStream, Stream, StreamKind};
