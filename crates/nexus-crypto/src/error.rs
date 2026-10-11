//! Error types for the crypto crate.

/// Failures in identity and signature handling.
#[derive(Debug, thiserror::Error)]
pub enum CryptoError {
    /// A public key, signature or identity blob is malformed.
    #[error("invalid {0}")]
    Invalid(&'static str),
    /// A signature did not verify.
    #[error("signature verification failed")]
    BadSignature,
    /// The key is a small-order / weak point and must never be trusted.
    #[error("weak or small-order public key")]
    WeakKey,
    /// An identity already exists in the store and would be overwritten.
    #[error("a device identity already exists")]
    AlreadyExists,
    /// The system random number generator failed.
    #[error("system random number generator failure: {0}")]
    Rng(String),
    /// Secret storage failed.
    #[error(transparent)]
    Store(#[from] StoreError),
}

/// Failures reading or writing secrets.
#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    /// The backing store is not available on this machine (no keychain, no Secret Service, …).
    #[error("secret store unavailable: {0}")]
    Unavailable(String),
    /// A stored secret exists but cannot be used (wrong size, bad checksum, bad magic).
    #[error("stored secret `{name}` is corrupt: {reason}")]
    Corrupt {
        /// Name of the secret.
        name: String,
        /// Why it is unusable.
        reason: &'static str,
    },
    /// The secret file is readable by other users or owned by someone else; refusing to use it.
    #[error("refusing to use `{path}`: {reason}")]
    InsecurePermissions {
        /// Offending path.
        path: String,
        /// What is wrong.
        reason: &'static str,
    },
    /// Underlying I/O failure.
    #[error("I/O error on secret store: {0}")]
    Io(#[from] std::io::Error),
    /// Any other backend-specific failure.
    #[error("secret store error: {0}")]
    Backend(String),
}
