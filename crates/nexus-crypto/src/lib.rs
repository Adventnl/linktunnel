//! Device identity, signatures and secret storage for Nexus Remote.
//!
//! This crate contains **no cryptographic primitives of its own**. It composes vetted
//! implementations:
//!
//! * Ed25519 / X25519: `ed25519-dalek`, `x25519-dalek`
//! * key derivation and hashing: BLAKE3
//! * OS secret storage: the `keyring` ecosystem
//!
//! What it adds is *protocol discipline*: one root seed per device with domain-separated
//! derivation, signatures bound to a purpose label so they cannot be replayed across contexts,
//! strict verification (small-order keys rejected), and key files that refuse to load when their
//! permissions are unsafe. Noise-based pairing and sealed envelopes build on this in
//! the Noise-based modules added in Phase 2.

#![forbid(unsafe_code)]

pub mod error;
pub mod identity;
pub mod noise;
pub mod provision;
pub mod store;

pub use ed25519_dalek::{Signature, VerifyingKey};
pub use error::{CryptoError, StoreError};
pub use identity::{
    DeviceId, DeviceIdentity, Fingerprint, IdentitySeed, PublicIdentity, SignContext,
    verify_with_key,
};
pub use provision::{
    IDENTITY_SECRET_NAME, Provisioned, create_identity, delete_identity, load_identity,
    load_or_create_identity,
};
pub use store::{
    DefaultStoreOptions, FileStore, KeyringStore, MemoryStore, SecretStore, StoreKind,
};
