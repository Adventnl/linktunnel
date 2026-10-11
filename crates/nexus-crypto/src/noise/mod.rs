//! Noise-protocol based flows: device pairing and sealed envelopes.
//!
//! Both use the [Noise Protocol Framework](https://noiseprotocol.org) through the `snow` crate;
//! nothing here implements a handshake by hand.
//!
//! * [`pairing`]: `Noise_XXpsk0_25519_ChaChaPoly_BLAKE2s` between two devices that have just met,
//!   authenticated by a one-time code (QR / 6 words) and confirmed by a human-compared short
//!   authentication string.
//! * [`envelope`]: `Noise_X_25519_ChaChaPoly_BLAKE2s` one-way sealed messages that paired devices
//!   exchange *through* the untrusted signaling server (wake commands, session requests).
//! * [`replay`]: sliding-window replay protection for envelopes.
//! * [`words`]: the 11-bit word encoding behind pairing codes and the SAS.

pub mod envelope;
pub mod pairing;
pub mod replay;
pub mod words;

/// Failures specific to the Noise-based protocols.
#[derive(Debug, thiserror::Error)]
pub enum NoiseError {
    /// The Noise handshake or transport failed (bad key, wrong PSK, corrupt message, …).
    #[error("noise protocol failure: {0}")]
    Protocol(String),
    /// A handshake message arrived (or was requested) out of turn.
    #[error("handshake message out of order")]
    OutOfOrder,
    /// The identity payload inside a handshake message is malformed.
    #[error("malformed peer payload")]
    BadPayload,
    /// The identity a peer presented is not the one authenticated by the handshake.
    #[error("peer identity does not match the authenticated Noise key")]
    IdentityMismatch,
    /// A message exceeds its size cap.
    #[error("message of {len} bytes exceeds the {max} byte limit")]
    TooLarge {
        /// Actual size.
        len: usize,
        /// Allowed maximum.
        max: usize,
    },
    /// The pairing code text is not valid.
    #[error("invalid pairing code: {0}")]
    BadCode(&'static str),
    /// An envelope's timestamp is outside the accepted window.
    #[error("envelope timestamp outside the accepted window")]
    Stale,
    /// An envelope counter has been seen before (or is too old to tell).
    #[error("replayed envelope")]
    Replay,
    /// The envelope's sender is not one of the supplied peers.
    #[error("envelope sender is not a known peer")]
    UnknownSender,
}

pub use pairing::{
    PairingChannel, PairingCode, PairingOutcome, PairingSession, Role, ShortAuthString,
};

impl From<snow::Error> for NoiseError {
    fn from(e: snow::Error) -> Self {
        Self::Protocol(e.to_string())
    }
}
