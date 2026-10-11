//! Who this device trusts, and the protocols that change that.
//!
//! * [`record`]: dual-signed [`PairingRecord`](nexus_proto::v1::PairingRecord)s. The signaling
//!   server stores and verifies them but can never fabricate one.
//! * [`revocation`]: signed revocations; either party can end a pairing unilaterally.
//! * [`store`]: the on-disk allow-list the agent consults before accepting any connection.
//! * [`flow`]: the pairing state machine (Noise handshake → human SAS check → both sign), free of
//!   I/O so it can be tested exhaustively and driven over any transport.

#![forbid(unsafe_code)]

pub mod error;
pub mod flow;
pub mod grants;
pub mod record;
pub mod revocation;
pub mod store;

pub use error::TrustError;
pub use record::VerifiedPairing;
pub use store::{PairedPeer, TrustStore};
