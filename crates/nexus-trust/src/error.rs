//! Errors for trust operations.

use nexus_crypto::CryptoError;

/// Why a record, revocation or pairing flow was rejected.
#[derive(Debug, thiserror::Error)]
pub enum TrustError {
    /// The record or revocation bytes are not a valid message.
    #[error("malformed {0}")]
    Malformed(&'static str),
    /// A field violates a protocol rule (ordering, sizes, limits).
    #[error("invalid {0}")]
    Invalid(&'static str),
    /// A signature did not verify.
    #[error("bad signature from {0}")]
    BadSignature(&'static str),
    /// The record's timestamp is too far from the local clock.
    #[error("timestamp outside the accepted window")]
    TimestampOutOfRange,
    /// The record is not about this device.
    #[error("record does not involve this device")]
    NotAParty,
    /// The pairing flow was used out of order or after finishing.
    #[error("pairing flow: {0}")]
    Flow(&'static str),
    /// The other side refused or the humans reported different codes.
    #[error("pairing aborted: {0}")]
    Aborted(&'static str),
    /// Persisting or loading the trust store failed.
    #[error("trust store I/O: {0}")]
    Io(#[from] std::io::Error),
    /// The trust store file is unreadable or has been tampered with.
    #[error("trust store corrupt: {0}")]
    Corrupt(String),
    /// A cryptographic operation failed.
    #[error(transparent)]
    Crypto(#[from] CryptoError),
}
