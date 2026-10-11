//! Long-term device identity.
//!
//! A device's identity is a single 32-byte random **root seed** held in the OS secret store.
//! Two independent keys are *derived* from it with BLAKE3's key-derivation mode (domain
//! separated by context string), so one secret protects both and the two keys never share
//! material:
//!
//! * an **Ed25519 signing key**: the device's identity on the wire. Its public half is also the
//!   QUIC/TLS endpoint identity (see `nexus-net`), so "who is this device" has exactly one
//!   answer everywhere;
//! * an **X25519 static key** for Noise handshakes (pairing, sealed envelopes), bound to the
//!   Ed25519 key by a signature so a peer that trusts the Ed25519 key can trust the X25519 key.
//!
//! Using separate keys for signing and Diffie-Hellman (instead of converting one into the
//! other) keeps each key to one purpose, which is what protocol analyses assume.

use std::fmt;
use std::str::FromStr;

use ed25519_dalek::{Signature, Signer, SigningKey, VerifyingKey};
use x25519_dalek::{PublicKey as X25519Public, StaticSecret};
use zeroize::Zeroizing;

use crate::error::CryptoError;

/// Length of the root seed in bytes.
pub const SEED_LEN: usize = 32;

// KDF context strings. They are part of the protocol: changing one changes every device's keys.
// Format follows the BLAKE3 recommendation: "[application] [date] [purpose]".
const KDF_ED25519: &str = "nexus-remote 2026-10-11 device identity ed25519 signing key";
const KDF_X25519: &str = "nexus-remote 2026-10-11 device identity x25519 noise static key";
const KDF_DEVICE_ID: &str = "nexus-remote 2026-10-11 device id from ed25519 public key";
const KDF_FINGERPRINT: &str = "nexus-remote 2026-10-11 fingerprint from ed25519 public key";
const SIGNATURE_PREFIX: &[u8] = b"nexus-remote/v1/signature\0";

/// What a signature is *for*. The label is mixed into every signed message, so a signature made
/// for one purpose can never be replayed as another (e.g. a signaling login proof cannot be
/// presented as a pairing record).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SignContext {
    /// Binds a device's X25519 key to its Ed25519 identity.
    KeyBinding,
    /// Proof of key possession when logging in to the signaling server.
    SignalAuth,
    /// Mutual agreement to pair two devices.
    PairingRecord,
    /// Revocation of a pairing.
    Revocation,
    /// Authenticity of a sealed envelope relayed through signaling.
    Envelope,
    /// An entry in the append-only audit log.
    AuditEntry,
    /// A target's grant of consent for a session.
    ConsentGrant,
    /// Binds a session to a handshake transcript.
    SessionBinding,
}

impl SignContext {
    /// Stable ASCII label mixed into signed messages.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::KeyBinding => "key-binding",
            Self::SignalAuth => "signal-auth",
            Self::PairingRecord => "pairing-record",
            Self::Revocation => "revocation",
            Self::Envelope => "envelope",
            Self::AuditEntry => "audit-entry",
            Self::ConsentGrant => "consent-grant",
            Self::SessionBinding => "session-binding",
        }
    }

    /// `prefix || len(label) || label || message`: the exact bytes that get signed.
    fn frame(self, message: &[u8]) -> Vec<u8> {
        let label = self.label().as_bytes();
        let mut out = Vec::with_capacity(SIGNATURE_PREFIX.len() + 1 + label.len() + message.len());
        out.extend_from_slice(SIGNATURE_PREFIX);
        // Labels are short compile-time constants (< 256 bytes); `try_from` documents that and
        // can never be reached with a real label (asserted in tests).
        out.push(u8::try_from(label.len()).unwrap_or(u8::MAX));
        out.extend_from_slice(label);
        out.extend_from_slice(message);
        out
    }
}

/// The 32-byte root secret from which a device's keys are derived.
pub struct IdentitySeed(Zeroizing<[u8; SEED_LEN]>);

impl IdentitySeed {
    /// Draws a fresh seed from the operating system's CSPRNG.
    pub fn generate() -> Result<Self, CryptoError> {
        let mut bytes = Zeroizing::new([0u8; SEED_LEN]);
        getrandom::fill(&mut bytes[..]).map_err(|e| CryptoError::Rng(e.to_string()))?;
        Ok(Self(bytes))
    }

    /// Wraps existing seed bytes (e.g. loaded from a secret store).
    #[must_use]
    pub fn from_bytes(bytes: [u8; SEED_LEN]) -> Self {
        Self(Zeroizing::new(bytes))
    }

    /// Parses a stored seed, rejecting any length other than [`SEED_LEN`].
    pub fn from_slice(bytes: &[u8]) -> Result<Self, CryptoError> {
        let array: [u8; SEED_LEN] = bytes
            .try_into()
            .map_err(|_| CryptoError::Invalid("identity seed length"))?;
        Ok(Self::from_bytes(array))
    }

    /// The raw seed, for writing to a secret store.
    #[must_use]
    pub fn as_bytes(&self) -> &[u8; SEED_LEN] {
        &self.0
    }
}

impl fmt::Debug for IdentitySeed {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("IdentitySeed([redacted])")
    }
}

/// Compact, stable identifier of a device: 128 bits of a hash of its Ed25519 public key.
///
/// It is for *addressing and display* only. Trust decisions always compare full public keys.
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct DeviceId([u8; Self::LEN]);

impl DeviceId {
    /// Length in bytes.
    pub const LEN: usize = 16;

    /// Derives the ID of the device owning `key`.
    #[must_use]
    pub fn from_public_key(key: &VerifyingKey) -> Self {
        let digest = blake3::derive_key(KDF_DEVICE_ID, key.as_bytes());
        let mut id = [0u8; Self::LEN];
        id.copy_from_slice(&digest[..Self::LEN]);
        Self(id)
    }

    /// Derives the ID from raw Ed25519 public-key bytes without parsing the curve point.
    /// Identical to [`from_public_key`](Self::from_public_key) for any valid key.
    #[must_use]
    pub fn from_ed25519_bytes(key: &[u8; 32]) -> Self {
        let digest = blake3::derive_key(KDF_DEVICE_ID, key);
        let mut id = [0u8; Self::LEN];
        id.copy_from_slice(&digest[..Self::LEN]);
        Self(id)
    }

    /// Wraps raw bytes (e.g. from a protobuf field).
    #[must_use]
    pub const fn from_bytes(bytes: [u8; Self::LEN]) -> Self {
        Self(bytes)
    }

    /// Parses raw bytes of exactly [`Self::LEN`] length.
    pub fn from_slice(bytes: &[u8]) -> Result<Self, CryptoError> {
        let array: [u8; Self::LEN] = bytes
            .try_into()
            .map_err(|_| CryptoError::Invalid("device id length"))?;
        Ok(Self(array))
    }

    /// Raw bytes.
    #[must_use]
    pub const fn as_bytes(&self) -> &[u8; Self::LEN] {
        &self.0
    }
}

impl fmt::Display for DeviceId {
    /// `NX-ABCD-EFGH-IJKL-MNOP-QRST-UV`: RFC 4648 base32 (no 0/1/8/9, so no look-alikes).
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let text = data_encoding::BASE32_NOPAD.encode(&self.0);
        f.write_str("NX")?;
        for chunk in text.as_bytes().chunks(4) {
            f.write_str("-")?;
            // Base32 output is ASCII, so chunk boundaries are always char boundaries.
            f.write_str(std::str::from_utf8(chunk).unwrap_or("?"))?;
        }
        Ok(())
    }
}

impl fmt::Debug for DeviceId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "DeviceId({self})")
    }
}

impl FromStr for DeviceId {
    type Err = CryptoError;

    /// Accepts the `Display` form case-insensitively; hyphens and spaces are ignored.
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let cleaned: String = s
            .chars()
            .filter(|c| !matches!(c, '-' | ' '))
            .map(|c| c.to_ascii_uppercase())
            .collect();
        let body = cleaned
            .strip_prefix("NX")
            .ok_or(CryptoError::Invalid("device id prefix"))?;
        let raw = data_encoding::BASE32_NOPAD
            .decode(body.as_bytes())
            .map_err(|_| CryptoError::Invalid("device id encoding"))?;
        Self::from_slice(&raw)
    }
}

/// A human-comparable digest of a device's public key (for reading aloud / eyeballing).
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct Fingerprint([u8; 32]);

impl Fingerprint {
    /// Full 32-byte digest.
    #[must_use]
    pub const fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }

    /// The first 100 bits as five groups of four base32 characters: `ABCD-EFGH-IJKL-MNOP-QRST`.
    #[must_use]
    pub fn short(&self) -> String {
        let text = data_encoding::BASE32_NOPAD.encode(&self.0[..13]);
        text.as_bytes()
            .chunks(4)
            .take(5)
            .filter_map(|c| std::str::from_utf8(c).ok())
            .collect::<Vec<_>>()
            .join("-")
    }
}

impl fmt::Display for Fingerprint {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.short())
    }
}

impl fmt::Debug for Fingerprint {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Fingerprint({})", self.short())
    }
}

/// The public, shareable half of a device identity.
///
/// Every constructor verifies the key binding and rejects weak keys, so a `PublicIdentity` in
/// hand is always internally consistent: the X25519 key really belongs to the Ed25519 key.
#[derive(Clone, PartialEq, Eq)]
pub struct PublicIdentity {
    ed25519: VerifyingKey,
    x25519: [u8; 32],
    binding: Signature,
}

/// Serialized size of a [`PublicIdentity`]: Ed25519 key + X25519 key + binding signature.
pub const PUBLIC_IDENTITY_LEN: usize = 32 + 32 + 64;

impl PublicIdentity {
    fn binding_message(x25519: &[u8; 32]) -> Vec<u8> {
        SignContext::KeyBinding.frame(x25519)
    }

    /// Reassembles and **verifies** an identity received from a peer.
    pub fn new(
        ed25519: VerifyingKey,
        x25519: [u8; 32],
        binding: Signature,
    ) -> Result<Self, CryptoError> {
        if ed25519.is_weak() {
            return Err(CryptoError::WeakKey);
        }
        // A degenerate X25519 key would make every DH output predictable.
        if x25519.iter().all(|b| *b == 0) {
            return Err(CryptoError::WeakKey);
        }
        ed25519
            .verify_strict(&Self::binding_message(&x25519), &binding)
            .map_err(|_| CryptoError::BadSignature)?;
        Ok(Self {
            ed25519,
            x25519,
            binding,
        })
    }

    /// Parses the [`to_bytes`](Self::to_bytes) encoding.
    pub fn from_bytes(bytes: &[u8]) -> Result<Self, CryptoError> {
        if bytes.len() != PUBLIC_IDENTITY_LEN {
            return Err(CryptoError::Invalid("public identity length"));
        }
        let mut ed = [0u8; 32];
        ed.copy_from_slice(&bytes[..32]);
        let mut x = [0u8; 32];
        x.copy_from_slice(&bytes[32..64]);
        let mut sig = [0u8; 64];
        sig.copy_from_slice(&bytes[64..]);
        let ed25519 = VerifyingKey::from_bytes(&ed)
            .map_err(|_| CryptoError::Invalid("ed25519 public key"))?;
        Self::new(ed25519, x, Signature::from_bytes(&sig))
    }

    /// `ed25519 (32) || x25519 (32) || binding signature (64)`.
    #[must_use]
    pub fn to_bytes(&self) -> [u8; PUBLIC_IDENTITY_LEN] {
        let mut out = [0u8; PUBLIC_IDENTITY_LEN];
        out[..32].copy_from_slice(self.ed25519.as_bytes());
        out[32..64].copy_from_slice(&self.x25519);
        out[64..].copy_from_slice(&self.binding.to_bytes());
        out
    }

    /// The Ed25519 identity key (also the transport endpoint identity).
    #[must_use]
    pub const fn ed25519(&self) -> &VerifyingKey {
        &self.ed25519
    }

    /// The Ed25519 public key bytes.
    #[must_use]
    pub fn ed25519_bytes(&self) -> [u8; 32] {
        self.ed25519.to_bytes()
    }

    /// The X25519 static key used in Noise handshakes.
    #[must_use]
    pub const fn x25519(&self) -> &[u8; 32] {
        &self.x25519
    }

    /// The short ID derived from the Ed25519 key.
    #[must_use]
    pub fn device_id(&self) -> DeviceId {
        DeviceId::from_public_key(&self.ed25519)
    }

    /// Human-comparable fingerprint of the Ed25519 key.
    #[must_use]
    pub fn fingerprint(&self) -> Fingerprint {
        Fingerprint(blake3::derive_key(KDF_FINGERPRINT, self.ed25519.as_bytes()))
    }

    /// Verifies a signature made by this device for `context`.
    pub fn verify(
        &self,
        context: SignContext,
        message: &[u8],
        signature: &Signature,
    ) -> Result<(), CryptoError> {
        verify_with_key(&self.ed25519, context, message, signature)
    }
}

impl fmt::Debug for PublicIdentity {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "PublicIdentity({})", self.device_id())
    }
}

/// Verifies `signature` over `message` for `context` with a bare Ed25519 key.
pub fn verify_with_key(
    key: &VerifyingKey,
    context: SignContext,
    message: &[u8],
    signature: &Signature,
) -> Result<(), CryptoError> {
    if key.is_weak() {
        return Err(CryptoError::WeakKey);
    }
    key.verify_strict(&context.frame(message), signature)
        .map_err(|_| CryptoError::BadSignature)
}

/// A device's full identity, including secrets. Never serialise this; persist the
/// [`IdentitySeed`] in a secret store and re-derive.
pub struct DeviceIdentity {
    signing: SigningKey,
    noise_secret: Zeroizing<[u8; 32]>,
    public: PublicIdentity,
}

impl DeviceIdentity {
    /// Derives the identity from a root seed. Deterministic: same seed, same keys.
    #[must_use]
    pub fn from_seed(seed: &IdentitySeed) -> Self {
        let ed_secret = Zeroizing::new(blake3::derive_key(KDF_ED25519, seed.as_bytes()));
        let noise_secret = Zeroizing::new(blake3::derive_key(KDF_X25519, seed.as_bytes()));

        let signing = SigningKey::from_bytes(&ed_secret);
        let x25519 = X25519Public::from(&StaticSecret::from(*noise_secret)).to_bytes();
        let binding = signing.sign(&PublicIdentity::binding_message(&x25519));
        let public = PublicIdentity {
            ed25519: signing.verifying_key(),
            x25519,
            binding,
        };

        Self {
            signing,
            noise_secret,
            public,
        }
    }

    /// Generates a brand-new random identity, returning the seed so it can be persisted.
    pub fn generate() -> Result<(Self, IdentitySeed), CryptoError> {
        let seed = IdentitySeed::generate()?;
        Ok((Self::from_seed(&seed), seed))
    }

    /// The shareable half.
    #[must_use]
    pub const fn public(&self) -> &PublicIdentity {
        &self.public
    }

    /// The short device ID.
    #[must_use]
    pub fn device_id(&self) -> DeviceId {
        self.public.device_id()
    }

    /// Signs `message` for `context`.
    #[must_use]
    pub fn sign(&self, context: SignContext, message: &[u8]) -> Signature {
        self.signing.sign(&context.frame(message))
    }

    /// The Ed25519 secret key bytes, for handing to the transport layer, which needs the same
    /// key as its endpoint identity. Treat the returned buffer as a secret (it zeroizes on drop).
    #[must_use]
    pub fn ed25519_secret_bytes(&self) -> Zeroizing<[u8; 32]> {
        Zeroizing::new(self.signing.to_bytes())
    }

    /// The X25519 static secret, for Noise handshakes within this crate.
    #[must_use]
    pub(crate) fn noise_secret(&self) -> &[u8; 32] {
        &self.noise_secret
    }
}

impl fmt::Debug for DeviceIdentity {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "DeviceIdentity({}, [secrets redacted])",
            self.device_id()
        )
    }
}

#[cfg(test)]
mod tests {
    use proptest::prelude::*;

    use super::*;

    fn fixed() -> DeviceIdentity {
        DeviceIdentity::from_seed(&IdentitySeed::from_bytes([0x42; SEED_LEN]))
    }

    /// Known-answer test: pins the derivation so an accidental change to a KDF context or
    /// algorithm (which would silently orphan every deployed device) fails CI.
    #[test]
    fn known_answer_derivation() {
        let id = fixed();
        let hex = |b: &[u8]| data_encoding::HEXLOWER.encode(b);
        assert_eq!(hex(&id.public().ed25519_bytes()), KAT_ED25519_PUB);
        assert_eq!(hex(id.public().x25519()), KAT_X25519_PUB);
        assert_eq!(id.device_id().to_string(), KAT_DEVICE_ID);
        assert_eq!(id.public().fingerprint().short(), KAT_FINGERPRINT);
    }

    // Values computed independently with Python (`blake3` + OpenSSL `cryptography`), not copied
    // from this crate's own output.
    const KAT_ED25519_PUB: &str =
        "1c84197e8cc54f71f104e103459214df60ea7dc1b2c06671c314e4985544a6e0";
    const KAT_X25519_PUB: &str = "80ba4853a8e165387c7ef61533736e3e5ca4b6bfdf2306fb79fdd23b7c5de128";
    const KAT_DEVICE_ID: &str = "NX-USZH-PGCU-4P7F-SSUA-4HLJ-2NAB-LU";
    const KAT_FINGERPRINT: &str = "6JM2-T3TD-Z7HL-L26K-VR67";

    #[test]
    fn derivation_is_deterministic_and_seed_sensitive() {
        let a = fixed();
        let b = fixed();
        assert_eq!(a.public(), b.public());
        let other = DeviceIdentity::from_seed(&IdentitySeed::from_bytes([0x43; SEED_LEN]));
        assert_ne!(a.device_id(), other.device_id());
    }

    #[test]
    fn signing_and_encryption_keys_are_independent() {
        let id = fixed();
        assert_ne!(&id.public().ed25519_bytes(), id.public().x25519());
        assert_ne!(
            id.ed25519_secret_bytes().as_slice(),
            id.noise_secret().as_slice()
        );
        assert_ne!(
            id.ed25519_secret_bytes().as_slice(),
            &[0x42; 32],
            "seed is never used directly"
        );
    }

    #[test]
    fn generated_identities_are_unique() {
        let (a, _) = DeviceIdentity::generate().unwrap();
        let (b, _) = DeviceIdentity::generate().unwrap();
        assert_ne!(a.device_id(), b.device_id());
    }

    #[test]
    fn sign_verify_roundtrip() {
        let id = fixed();
        let sig = id.sign(SignContext::PairingRecord, b"hello");
        id.public()
            .verify(SignContext::PairingRecord, b"hello", &sig)
            .unwrap();
        assert!(matches!(
            id.public()
                .verify(SignContext::PairingRecord, b"hellp", &sig),
            Err(CryptoError::BadSignature)
        ));
    }

    #[test]
    fn sign_context_labels_fit_the_length_prefix() {
        for ctx in [
            SignContext::KeyBinding,
            SignContext::SignalAuth,
            SignContext::PairingRecord,
            SignContext::Revocation,
            SignContext::Envelope,
            SignContext::AuditEntry,
            SignContext::ConsentGrant,
            SignContext::SessionBinding,
        ] {
            assert!(ctx.label().len() < 64, "{ctx:?}");
            assert!(ctx.label().is_ascii(), "{ctx:?}");
        }
    }

    #[test]
    fn signatures_are_domain_separated() {
        let id = fixed();
        let sig = id.sign(SignContext::SignalAuth, b"nonce");
        for other in [
            SignContext::PairingRecord,
            SignContext::Revocation,
            SignContext::Envelope,
            SignContext::AuditEntry,
            SignContext::KeyBinding,
            SignContext::ConsentGrant,
            SignContext::SessionBinding,
        ] {
            assert!(
                id.public().verify(other, b"nonce", &sig).is_err(),
                "signature for SignalAuth verified as {other:?}"
            );
        }
        id.public()
            .verify(SignContext::SignalAuth, b"nonce", &sig)
            .unwrap();
    }

    #[test]
    fn signature_from_another_device_is_rejected() {
        let a = fixed();
        let b = DeviceIdentity::from_seed(&IdentitySeed::from_bytes([1; SEED_LEN]));
        let sig = b.sign(SignContext::Envelope, b"m");
        assert!(
            a.public()
                .verify(SignContext::Envelope, b"m", &sig)
                .is_err()
        );
    }

    #[test]
    fn public_identity_bytes_roundtrip() {
        let id = fixed();
        let bytes = id.public().to_bytes();
        assert_eq!(PublicIdentity::from_bytes(&bytes).unwrap(), *id.public());
    }

    #[test]
    fn public_identity_rejects_tampering() {
        let id = fixed();
        let good = id.public().to_bytes();
        // Flip a bit in each region: ed25519 key, x25519 key, signature.
        for idx in [3, 40, 100] {
            let mut bad = good;
            bad[idx] ^= 1;
            assert!(
                PublicIdentity::from_bytes(&bad).is_err(),
                "tamper at byte {idx} accepted"
            );
        }
        assert!(PublicIdentity::from_bytes(&good[..127]).is_err());
        assert!(PublicIdentity::from_bytes(&[]).is_err());
    }

    #[test]
    fn x25519_key_swap_attack_is_rejected() {
        // An attacker substitutes their own X25519 key under a victim's Ed25519 key.
        let victim = fixed();
        let attacker = DeviceIdentity::from_seed(&IdentitySeed::from_bytes([9; SEED_LEN]));
        let mut forged = victim.public().to_bytes();
        forged[32..64].copy_from_slice(attacker.public().x25519());
        assert!(matches!(
            PublicIdentity::from_bytes(&forged),
            Err(CryptoError::BadSignature)
        ));
    }

    #[test]
    fn weak_keys_are_rejected() {
        // The identity point (small order) as an Ed25519 public key.
        let mut identity_point = [0u8; 32];
        identity_point[0] = 1;
        let key = VerifyingKey::from_bytes(&identity_point).unwrap();
        let sig = fixed().sign(SignContext::KeyBinding, &[1; 32]);
        assert!(matches!(
            PublicIdentity::new(key, [7; 32], sig),
            Err(CryptoError::WeakKey)
        ));
        assert!(matches!(
            PublicIdentity::new(*fixed().public().ed25519(), [0; 32], sig),
            Err(CryptoError::WeakKey)
        ));
        assert!(matches!(
            verify_with_key(&key, SignContext::Envelope, b"x", &sig),
            Err(CryptoError::WeakKey)
        ));
    }

    #[test]
    fn device_id_from_raw_bytes_matches_from_public_key() {
        let id = fixed();
        assert_eq!(
            DeviceId::from_ed25519_bytes(&id.public().ed25519_bytes()),
            id.device_id()
        );
    }

    #[test]
    fn device_id_display_parse_roundtrip() {
        let id = fixed().device_id();
        let text = id.to_string();
        assert!(text.starts_with("NX-"), "{text}");
        assert_eq!(text.len(), "NX".len() + 26 + 7, "{text}");
        assert_eq!(text.parse::<DeviceId>().unwrap(), id);
        assert_eq!(text.to_lowercase().parse::<DeviceId>().unwrap(), id);
        assert_eq!(text.replace('-', "").parse::<DeviceId>().unwrap(), id);
    }

    #[test]
    fn device_id_parse_rejects_garbage() {
        for bad in [
            "",
            "NX",
            "NX-",
            "AB-CDEF",
            "NX-0000-0000",
            "NX-AAAA",
            "XX-ABCD",
        ] {
            assert!(bad.parse::<DeviceId>().is_err(), "`{bad}` parsed");
        }
        // One character too long.
        let long = format!("{}A", fixed().device_id());
        assert!(long.parse::<DeviceId>().is_err());
    }

    #[test]
    fn debug_never_leaks_secrets() {
        let seed = IdentitySeed::from_bytes([0x42; SEED_LEN]);
        let id = DeviceIdentity::from_seed(&seed);
        let dump = format!("{seed:?} {id:?}");
        assert!(dump.contains("redacted"));
        assert!(!dump.contains("66, 66"), "seed bytes leaked: {dump}");
        assert!(!dump.contains("4242"), "seed bytes leaked: {dump}");
    }

    #[test]
    fn seed_length_is_validated() {
        assert!(IdentitySeed::from_slice(&[0; 31]).is_err());
        assert!(IdentitySeed::from_slice(&[0; 33]).is_err());
        assert!(IdentitySeed::from_slice(&[0; 32]).is_ok());
    }

    proptest! {
        #[test]
        fn public_identity_decode_never_panics(data in proptest::collection::vec(any::<u8>(), 0..200)) {
            let _ = PublicIdentity::from_bytes(&data);
        }

        #[test]
        fn device_id_parse_never_panics(s in "\\PC{0,64}") {
            let _ = s.parse::<DeviceId>();
        }

        #[test]
        fn any_seed_yields_consistent_identity(seed in any::<[u8; 32]>()) {
            let id = DeviceIdentity::from_seed(&IdentitySeed::from_bytes(seed));
            prop_assert!(PublicIdentity::from_bytes(&id.public().to_bytes()).is_ok());
            let s = id.device_id().to_string();
            prop_assert_eq!(s.parse::<DeviceId>().unwrap(), id.device_id());
        }
    }
}
